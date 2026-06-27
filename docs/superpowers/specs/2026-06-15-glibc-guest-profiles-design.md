# glibc / manylinux Guest Profiles Design

## Context

Every abox guest profile (`base`, `node`, `python`, `rust`) is assembled from a
single **Alpine minirootfs (musl)** base via `apk` (`scripts/build_rootfs.sh`).
For the `python` profile this means `pip`/`uv` only ever see `musllinux` wheels
or fall back to building from source — the enormous `manylinux` (glibc) wheel
ecosystem (numpy/pandas/scipy/pyarrow/torch and most native-extension packages)
does not apply. The base already installs `gcompat`, but that only lets some
prebuilt glibc *binaries* run; it does **not** change pip's platform tag, which
stays `musllinux`, so wheel selection is unaffected.

This design adds a **libc axis** to guest profiles: a parallel Debian/glibc
build path alongside the existing Alpine/musl path, and ships one new profile,
`python-glibc`, on which `manylinux` wheels resolve natively. The existing musl
profiles are left byte-for-byte unchanged.

## Goals

1. A `python-glibc` profile where `pip`/`uv` install `manylinux` wheels.
2. Generalize the glibc base so `node-glibc`/`rust-glibc` are later one-liners
   (build the axis now; ship only `python-glibc`).
3. **Zero change** to the existing musl profiles — provably identical images.
4. `runner.sh` and `init.sh` stay unchanged across both bases.
5. Builds stay **unprivileged** (no `apt`-in-chroot) and **reproducible /
   attestable**, matching the repo's existing rootfs-build attestation culture.

## Non-Goals

- Renaming or rebasing existing profiles (`python` stays musl).
- Shipping `node-glibc` / `rust-glibc` now (only the shared infrastructure).
- A privileged `debootstrap` / `apt`-in-chroot build path.
- Solving wheels for musl (that is what `python-glibc` is for).

---

## 1. Profile model

`EnvironmentProfile` (`crates/abox-core/src/project.rs`) gains one variant and a
libc accessor. Each profile is conceptually `(toolchain, libc)`.

- Add `PythonGlibc` → serialized/`as_str`/`FromStr` as `"python-glibc"`.
- Add `fn libc(&self) -> Libc` returning `Musl` for the existing four and
  `Glibc` for `PythonGlibc` (new `enum Libc { Musl, Glibc }`).
- `toolchain_summary(PythonGlibc)` = "python3, uv, and pip3 (glibc / manylinux
  wheels)".
- `supports_cache`: `(PythonGlibc, "pip" | "uv")` is true (same caches as
  `Python`), so durable pip/uv caches work for it.
- `recommended_for_cache` is **unchanged** — `pip`/`uv` still recommend the
  musl `Python` by default; `python-glibc` is an explicit opt-in (it is larger).
- `uses_dedicated_image` is already true for everything but `Base`, so
  `PythonGlibc` installs to `profiles/python-glibc/rootfs.raw` for free.

`build_rootfs.sh` and `bootstrap_vm.sh` profile validation lists gain
`python-glibc`.

## 2. glibc base production (Docker build + export, on the host)

The Debian rootfs is produced by Docker on the **host** (where Docker already
runs) and consumed by the existing Alpine builder as a tarball — **no
docker-in-docker** (rec 3).

- New `scripts/glibc/python-glibc.Dockerfile`:
  - `FROM debian:bookworm-slim@sha256:<pinned-digest>` (rec 4 — pinned, not a
    moving tag).
  - `apt-get install --no-install-recommends` the shared runtime
    (`ca-certificates`, `socat`) + python toolchain
    (`python3 python3-pip python3-venv`); install `uv` (official installer or
    `pip install uv`); install a pinned Node major (NodeSource or a pinned node
    tarball) so the in-guest Claude/Codex CLI runtime matches the musl profiles
    (rec 5).
  - Fetch the `gosu` release binary and **GPG/checksum-verify** it (it is not in
    the Debian archive), then `ln -s /usr/bin/gosu /usr/local/bin/su-exec`
    (rec 1) so `runner.sh`'s `su-exec abox:abox …` works unchanged.
  - Slim aggressively: `--no-install-recommends`, `apt-get clean`,
    `rm -rf /var/lib/apt/lists/* /usr/share/doc /usr/share/man`, drop
    `__pycache__` (rec 7).
- `bootstrap_vm.sh` adds `produce_glibc_base(<profile>)`: `docker build` the
  profile Dockerfile, `docker create` + `docker export` to
  `$ABOX_VM_DIR/<profile>-rootfs.tar.gz`, cached by a content hash of the
  Dockerfile (+ pinned digest) so re-runs are no-ops — mirroring the
  checksummed Alpine download.

The Debian base provides glibc 2.36, which satisfies `manylinux_2_28` and
`manylinux_2_34` — i.e. essentially all current manylinux wheels. The
`manylinux_2_28` baseline is stated in the docs.

## 3. `build_rootfs.sh` — parameterize by libc; share the abox staging

`build_rootfs.sh` branches on the profile's libc. The **abox-specific staging**
is factored into one shared function both branches call, so the diff is small
and the musl path is provably unchanged.

- `profile_libc()` → `musl` | `glibc`.
- **musl branch:** today's flow verbatim (extract `alpine-minirootfs.tar.gz`,
  `apk … add`, busybox `addgroup`/`adduser`). Untouched.
- **glibc branch:** extract `<profile>-rootfs.tar.gz`; create the `abox` user
  via `chroot "$stage" groupadd -g 1000 abox` + `useradd -u 1000 -g abox -m -s
  /bin/bash abox` (Debian tools, offline — no mounts needed).
- **Shared staging** (both branches): install `abox-shim` + `git`/`gh`/`aws`
  symlinks; install `/sbin/init`; `npm install --global --prefix` the
  Claude/Codex CLIs using the **builder's** npm (base-agnostic — it writes JS +
  fixes the shebang); size and `mkfs.ext4` the image from staged content + 512
  MiB headroom (unchanged sizing).
- **CA bundle is libc-aware** (rec 2): musl keeps the
  `cat …/mozilla/*.crt > ca-certificates.crt` glob; glibc relies on Debian's
  `ca-certificates` package output at `/etc/ssl/certs/ca-certificates.crt`.
  `init.sh`'s later append of the abox root CA is already base-agnostic.

`runner.sh` (generated in `crates/abox-core/src/boot_meta.rs`) and `init.sh` are
**unchanged**: `su-exec` is satisfied by the gosu symlink, and `init.sh` uses
only kernel mounts and the standard `/etc/ssl/certs` path.

## 4. Reproducibility & attestation (rec 4)

- The Debian base is pinned by digest in the Dockerfile.
- The per-profile `rootfs.raw.inputs` stamp records, in addition to today's
  fields: the base image digest, the **resolved** apt package versions
  (`dpkg-query -W` captured at build), the pinned node + uv + gosu versions, and
  the Dockerfile hash. This gives glibc profiles the same input-fingerprint
  attestation the musl profiles already have, so `check-rootfs` / freshness and
  the pre-release attestation flow work identically.

## 5. Testing

- **Unit (abox-core):** `EnvironmentProfile` round-trip for `python-glibc`
  (`FromStr`/`as_str`/serde), `libc()` mapping, `supports_cache(PythonGlibc,
  "pip"|"uv")`, and that `recommended_for_cache("pip")` still returns musl
  `Python` (opt-in preserved).
- **VM (tier-vm) — the proof (rec 6):** the primary, robust discriminator is
  the interpreter's **supported wheel platform tags**, which is exactly what
  blocks manylinux wheels on musl and is independent of any package's shifting
  wheel matrix. Assert that in the `python-glibc` guest `pip debug --verbose`
  lists `manylinux_*` compatible tags (and **no** `musllinux_*`), and that the
  musl `python` guest lists `musllinux_*` (and no `manylinux_*`). As an
  end-to-end smoke, additionally `pip install --only-binary=:all:` a known
  manylinux package and `import` it on `python-glibc`; this is asserted to
  **succeed** but not used as the discriminator (so it can't silently rot if the
  package later ships a `musllinux` wheel). `pip debug` is built into pip, so no
  extra guest dependency is required.
- **Build:** the glibc rootfs build runs wherever Docker is available
  (`bootstrap`, `e2e_test.sh` profile path). CI has Docker (build) but no KVM
  (the boot assertion is tier-vm, attested locally like the rest of the VM
  suite).

## 6. Documentation

- `README.md` — profile list gains `python-glibc` with a one-line "manylinux
  wheels" note and the larger-image caveat.
- `docs/explainer.md` — the libc axis and why musl blocks manylinux wheels.
- Guest-profile / config docs + `templates/config.example.toml` — show
  `environment.profile = "python-glibc"`.
- `docs/future-work.md` — record `node-glibc`/`rust-glibc` as now-cheap
  follow-ons.
- `AGENTS.md` — note the second build path if any `just`/build recipe changes.

## 7. Future work (explicitly deferred)

- **Naming symmetry:** a `python-musl` alias so the axis reads consistently;
  do not rename existing profiles now.
- **`node-glibc` / `rust-glibc`:** one Dockerfile + enum arm each, once there's
  demand.
- **`abox init` / `doctor` hint:** steer data-science users toward
  `python-glibc`. Small UX follow-up.

---

## Decisions recorded

- **Base:** `debian:bookworm-slim`, pinned by digest (glibc 2.36 →
  `manylinux_2_28`/`_2_34`).
- **Shape:** new `python-glibc` profile; existing musl `python` kept; libc axis
  generalized but only `python-glibc` ships now.
- **Build mechanism:** per-glibc-profile Dockerfile → `docker build` + `docker
  export` on the host → tarball consumed by the existing Alpine builder. No
  `apt`-in-chroot, no docker-in-docker.
- **Privilege drop:** verified `gosu` binary, symlinked to `su-exec`
  (Debian-only); Alpine path untouched; `runner.sh` unchanged.
- **CA bundle:** libc-aware staging branch.
- **Node:** pinned to the same major across both libc variants.
- **Reproducibility:** digest-pinned base + resolved package versions recorded
  in the `.inputs` stamp.
- **Test discriminator:** a `manylinux`-only (no `musllinux` wheel) package, not
  `numpy`.
