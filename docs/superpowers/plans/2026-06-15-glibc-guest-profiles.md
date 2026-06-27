# glibc / manylinux Guest Profiles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `python-glibc` guest profile (Debian/glibc base) where `pip`/`uv` resolve `manylinux` wheels, via a generalized libc axis, leaving the existing musl profiles unchanged.

**Architecture:** Introduce `EnvironmentProfile::PythonGlibc` with a `Libc` accessor. Produce the Debian rootfs with a per-profile Dockerfile that `bootstrap_vm.sh` `docker build`+`export`s on the host (unprivileged, no docker-in-docker); `build_rootfs.sh` consumes that tarball through a glibc branch that shares the abox staging (shim/init/CLIs/mkfs) with the musl branch. `gosu` is symlinked to `su-exec` so `runner.sh`/`init.sh` stay byte-identical.

**Tech Stack:** Rust (serde, clap), Bash, Docker, Debian bookworm-slim, Cloud Hypervisor + virtiofsd, ext4.

**Spec:** `docs/superpowers/specs/2026-06-15-glibc-guest-profiles-design.md`

**Branch:** `feat/glibc-guest-profiles` (already created; spec committed there).

**Quality gate (run before every Rust commit):**
```bash
RUSTUP_TOOLCHAIN=stable cargo fmt --all
RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=stable cargo test --workspace
```
(Local env pins `RUSTUP_TOOLCHAIN=1.94.0`; CI uses stable — always run clippy/test with `RUSTUP_TOOLCHAIN=stable`.)

---

## File Structure

**Create:**
- `scripts/glibc/python-glibc.Dockerfile` — Debian/glibc base for the `python-glibc` profile.

**Modify:**
- `crates/abox-core/src/project.rs` — `Libc` enum, `PythonGlibc` variant, `libc()`, exhaustive-match arms, unit tests.
- `crates/abox-cli/src/commands/project.rs` — `ProjectProfileArg::PythonGlibc`.
- `crates/abox-cli/src/commands/init.rs` — `InitProfileArg::PythonGlibc`.
- `crates/abox-cli/src/commands/doctor.rs` — profile arrays.
- `scripts/bootstrap_vm.sh` — `add_profile` validation + `produce_glibc_base()` + build-loop dispatch.
- `scripts/build_rootfs.sh` — profile validation, `profile_libc()`, shared staging fn, glibc branch, `.inputs` stamp.
- `scripts/local/e2e_test.sh` — gated `python-glibc` manylinux assertion.
- `README.md`, `docs/explainer.md`, `templates/config.example.toml`, `docs/future-work.md`, `AGENTS.md` — docs.

---

## Task 1: Profile model — `Libc` + `PythonGlibc` (abox-core)

**Files:** Modify `crates/abox-core/src/project.rs`

- [ ] **Step 1: Write failing tests**

In `project.rs`'s `#[cfg(test)] mod tests`, add:
```rust
    #[test]
    fn python_glibc_profile_round_trips_and_is_glibc() {
        use std::str::FromStr;
        let p = EnvironmentProfile::from_str("python-glibc").unwrap();
        assert_eq!(p, EnvironmentProfile::PythonGlibc);
        assert_eq!(p.as_str(), "python-glibc");
        assert_eq!(p.to_string(), "python-glibc");
        assert_eq!(p.libc(), Libc::Glibc);
        assert_eq!(EnvironmentProfile::Python.libc(), Libc::Musl);
        assert_eq!(EnvironmentProfile::Base.libc(), Libc::Musl);
    }

    #[test]
    fn python_glibc_supports_python_caches_but_is_not_the_default() {
        // Same caches as musl python...
        assert!(EnvironmentProfile::PythonGlibc.supports_cache("pip"));
        assert!(EnvironmentProfile::PythonGlibc.supports_cache("uv"));
        assert!(!EnvironmentProfile::PythonGlibc.supports_cache("npm"));
        // ...but the recommended profile for pip/uv stays the smaller musl python.
        assert_eq!(
            EnvironmentProfile::recommended_for_cache("pip"),
            Some(EnvironmentProfile::Python)
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `RUSTUP_TOOLCHAIN=stable cargo test -p abox-core python_glibc 2>&1 | head -20`
Expected: FAIL — `Libc` / `PythonGlibc` not found.

- [ ] **Step 3: Add the `Libc` enum**

In `project.rs`, immediately before `pub enum EnvironmentProfile {` (line ~66), add:
```rust
/// The C library a guest profile's rootfs is built against. Determines whether
/// Python wheels resolve as `manylinux` (glibc) or `musllinux` (musl).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Libc {
    /// Alpine-based musl rootfs (the historical default).
    Musl,
    /// Debian-based glibc rootfs (manylinux wheels).
    Glibc,
}
```

- [ ] **Step 4: Add the variant + update every match**

In `enum EnvironmentProfile`, after `Rust,` add:
```rust
    /// Python-focused guest image on a glibc (Debian) base, so `pip`/`uv`
    /// resolve `manylinux` wheels.
    PythonGlibc,
```
Add the `libc()` method inside `impl EnvironmentProfile` (e.g. after `toolchain_summary`):
```rust
    /// The C library this profile's rootfs is built against.
    pub fn libc(&self) -> Libc {
        match self {
            Self::PythonGlibc => Libc::Glibc,
            Self::Base | Self::Node | Self::Python | Self::Rust => Libc::Musl,
        }
    }
```
`toolchain_summary` — add arm:
```rust
            Self::PythonGlibc => "python3, uv, and pip3 (glibc / manylinux wheels)",
```
`supports_cache` — add `PythonGlibc` to the pip/uv arm:
```rust
        matches!(
            (self, cache),
            (Self::Base, _)
                | (Self::Node, "npm")
                | (Self::Python | Self::PythonGlibc, "pip" | "uv")
                | (Self::Rust, "cargo")
        )
```
`as_str` — add arm:
```rust
            Self::PythonGlibc => "python-glibc",
```
`FromStr` — add arm before the `other =>` catch-all:
```rust
            "python-glibc" => Ok(Self::PythonGlibc),
```
and update the catch-all error text to mention it:
```rust
            other => anyhow::bail!(
                "unknown environment profile {other:?}; expected base, node, python, python-glibc, or rust"
            ),
```
Leave `recommended_for_cache` unchanged (pip/uv still recommend musl `Python`).

- [ ] **Step 5: Run to verify pass + gate**

Run:
```bash
RUSTUP_TOOLCHAIN=stable cargo test -p abox-core python_glibc
RUSTUP_TOOLCHAIN=stable cargo clippy -p abox-core --all-targets -- -D warnings
```
Expected: tests PASS, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/abox-core/src/project.rs
git commit -m "feat(profile): add PythonGlibc variant + Libc axis"
```

---

## Task 2: Thread `PythonGlibc` through the CLI surfaces

**Files:** Modify `crates/abox-cli/src/commands/project.rs`, `init.rs`, `doctor.rs`

- [ ] **Step 1: Build to list the breaks**

Run: `RUSTUP_TOOLCHAIN=stable cargo build -p abox-cli 2>&1 | grep -E "non-exhaustive|match|PythonGlibc|-->.*\.rs:[0-9]" | head`
Expected: exhaustiveness errors in `project.rs` (`ProjectProfileArg` → `Self` match), `init.rs` (`InitProfileArg::as_str`), `doctor.rs` (profile arrays).

- [ ] **Step 2: `ProjectProfileArg` (project.rs)**

In `crates/abox-cli/src/commands/project.rs`, add to `enum ProjectProfileArg` (after `Rust,`, ~line 43):
```rust
    PythonGlibc,
```
and to its `From`/match into `EnvironmentProfile` (the arm block at ~line 207-210), add:
```rust
            ProjectProfileArg::PythonGlibc => Self::PythonGlibc,
```
(clap renders the value as `python-glibc` automatically via kebab-case.)

- [ ] **Step 3: `InitProfileArg` (init.rs)**

In `crates/abox-cli/src/commands/init.rs`, add `PythonGlibc` to the `InitProfileArg` enum definition (find `enum InitProfileArg` and add the variant after `Rust`), then add to its `as_str` match (~line 252-255):
```rust
            Self::PythonGlibc => "python-glibc",
```

- [ ] **Step 4: `doctor.rs` profile arrays**

In `crates/abox-cli/src/commands/doctor.rs`, both arrays that list profiles (~line 378-381 and ~403-406) get an added element:
```rust
        EnvironmentProfile::PythonGlibc,
```

- [ ] **Step 5: Build + smoke the CLI parse**

Run:
```bash
RUSTUP_TOOLCHAIN=stable cargo build -p abox-cli
RUSTUP_TOOLCHAIN=stable cargo run -q -p abox-cli -- project set-profile --help 2>&1 | grep -i "python-glibc" || true
```
Expected: clean build; the profile value appears in help (clap lists `python-glibc`).

- [ ] **Step 6: Full gate + commit**

Run:
```bash
RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=stable cargo test --workspace
```
Then:
```bash
git add crates/abox-cli/src/commands/project.rs crates/abox-cli/src/commands/init.rs crates/abox-cli/src/commands/doctor.rs
git commit -m "feat(cli): accept python-glibc profile across project/init/doctor"
```

---

## Task 3: Allow `python-glibc` in the build scripts' validation

**Files:** Modify `scripts/bootstrap_vm.sh`, `scripts/build_rootfs.sh`

- [ ] **Step 1: bootstrap_vm.sh `add_profile`**

In `scripts/bootstrap_vm.sh`, update the `add_profile` case (~line 49) from:
```bash
        base|node|python|rust)
```
to:
```bash
        base|node|python|python-glibc|rust)
```
and the error text on the next lines to mention `python-glibc`.

- [ ] **Step 2: build_rootfs.sh validation**

In `scripts/build_rootfs.sh`, update the `ABOX_PROFILE` case (~line 26-33):
```bash
case "$ABOX_PROFILE" in
    base|node|python|python-glibc|rust)
        ;;
    *)
        echo "ERROR: unsupported ABOX_PROFILE '$ABOX_PROFILE' (expected base, node, python, python-glibc, or rust)" >&2
        exit 1
        ;;
esac
```

- [ ] **Step 3: Verify validation accepts it**

Run:
```bash
ABOX_VM_DIR=/tmp/x SHIM_BIN=/bin/true GUEST_INIT=/bin/true ABOX_PROFILE=python-glibc bash -n scripts/build_rootfs.sh && echo "syntax ok"
bash -c 'source <(sed -n "1,64p" scripts/bootstrap_vm.sh) 2>/dev/null; true' ; echo "bootstrap parses"
```
Expected: `syntax ok` (the case now lists `python-glibc`; full run is exercised in later tasks).

- [ ] **Step 4: Commit**

```bash
git add scripts/bootstrap_vm.sh scripts/build_rootfs.sh
git commit -m "build: allow python-glibc in profile validation"
```

---

## Task 4: The Debian/glibc Dockerfile

**Files:** Create `scripts/glibc/python-glibc.Dockerfile`

- [ ] **Step 1: Resolve the pinned base digest**

Run:
```bash
docker pull debian:bookworm-slim
docker inspect --format='{{index .RepoDigests 0}}' debian:bookworm-slim
```
Note the `debian@sha256:...` digest; use it in the `FROM` line below.

- [ ] **Step 2: Create the Dockerfile**

Create `scripts/glibc/python-glibc.Dockerfile`:
```dockerfile
# python-glibc.Dockerfile — Debian/glibc base for the abox `python-glibc` guest
# profile. Built and `docker export`ed on the host by bootstrap_vm.sh; the
# resulting tarball is consumed by build_rootfs.sh like alpine-minirootfs.tar.gz.
#
# Pin the base by digest (resolve with:
#   docker inspect --format='{{index .RepoDigests 0}}' debian:bookworm-slim ).
FROM debian:bookworm-slim@sha256:REPLACE_WITH_RESOLVED_DIGEST

# gosu (CLI-identical to su-exec; not in the Debian archive — fetched + verified)
ARG GOSU_VERSION=1.17
# Match the musl profile's Node major (Alpine 3.19 ships Node 20).
ARG NODE_MAJOR=20
ENV DEBIAN_FRONTEND=noninteractive

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg socat \
        python3 python3-pip python3-venv; \
    # Node, pinned major, via NodeSource (parity with the musl profile).
    curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash -; \
    apt-get install -y --no-install-recommends nodejs; \
    # uv via the standalone installer into a system path (avoids PEP 668).
    curl -fsSL https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh; \
    # gosu, GPG-verified, exposed as su-exec so runner.sh stays unchanged.
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
    # Slim: drop the build-only tools, caches, docs, and pyc.
    apt-get purge -y --auto-remove curl gnupg; \
    apt-get clean; \
    rm -rf /var/lib/apt/lists/* /usr/share/doc/* /usr/share/man/* /var/log/*; \
    find / -type d -name __pycache__ -prune -exec rm -rf {} + 2>/dev/null || true
```

- [ ] **Step 3: Build + verify the image satisfies the contract**

Run:
```bash
docker build -f scripts/glibc/python-glibc.Dockerfile -t abox-python-glibc:test scripts/glibc
docker run --rm abox-python-glibc:test sh -c 'su-exec root id && uv --version && node --version && pip debug --verbose 2>/dev/null | grep -c manylinux'
```
Expected: `su-exec` runs (it's gosu), `uv`/`node` print versions, and the `manylinux` tag count is `>0` (the whole point). If the count is 0, the base is not glibc — stop and investigate.

- [ ] **Step 4: Commit**

```bash
git add scripts/glibc/python-glibc.Dockerfile
git commit -m "build(glibc): add python-glibc Debian base Dockerfile (verified gosu, pinned node)"
```

---

## Task 5: `bootstrap_vm.sh` — produce the glibc base tarball

**Files:** Modify `scripts/bootstrap_vm.sh`

- [ ] **Step 1: Add `produce_glibc_base()` and dispatch**

In `scripts/bootstrap_vm.sh`, before the Phase 5 build loop (~line 287), add:
```bash
# Build a glibc profile's Debian base on the host (Docker handles apt/network
# unprivileged) and `docker export` it to a rootfs tarball consumed by
# build_rootfs.sh — no docker-in-docker. Cached by the Dockerfile's content hash.
produce_glibc_base() {
    local profile="$1"
    local dockerfile="$REPO_ROOT/scripts/glibc/${profile}.Dockerfile"
    local out="$ABOX_VM_DIR/${profile}-rootfs.tar.gz"
    local stamp="$out.dockerfile.sha256"
    [ -f "$dockerfile" ] || { echo "ERROR: missing $dockerfile" >&2; exit 1; }
    local want
    want="$(sha256sum "$dockerfile" | cut -d' ' -f1)"
    if [[ -f "$out" && -f "$stamp" && "$(cat "$stamp")" == "$want" ]]; then
        echo "  glibc base for '$profile' is up to date (cached)"
        return
    fi
    echo "  building glibc base for '$profile' via Docker..."
    local tag="abox-rootfs-${profile}:$(printf '%s' "$want" | cut -c1-16)"
    docker build -f "$dockerfile" -t "$tag" "$REPO_ROOT/scripts/glibc"
    local cid
    cid="$(docker create "$tag")"
    docker export "$cid" | gzip > "$out"
    docker rm "$cid" >/dev/null
    printf '%s\n' "$want" > "$stamp"
    echo "  glibc base for '$profile' -> $out ($(du -h "$out" | cut -f1))"
}
```

- [ ] **Step 2: Call it for glibc profiles in the build loop**

Change the Phase 5 loop (~line 288-295) so glibc profiles get their base produced first:
```bash
for profile in "${PROFILES[@]}"; do
    echo "  profile: $profile"
    case "$profile" in
        *-glibc) produce_glibc_base "$profile" ;;
    esac
    SHIM_BIN="$SHIM_BIN" \
    ABOX_VM_DIR="$ABOX_VM_DIR" \
    ABOX_PROFILE="$profile" \
    GUEST_INIT="$REPO_ROOT/guest/init.sh" \
        "$REPO_ROOT/scripts/build_rootfs.sh"
done
```

- [ ] **Step 3: Verify the tarball is produced**

Run (Docker required):
```bash
docker build -f scripts/glibc/python-glibc.Dockerfile -t abox-rootfs-python-glibc:probe scripts/glibc >/dev/null
cid=$(docker create abox-rootfs-python-glibc:probe); docker export "$cid" | gzip > /tmp/python-glibc-rootfs.tar.gz; docker rm "$cid" >/dev/null
tar -tzf /tmp/python-glibc-rootfs.tar.gz | grep -E "usr/local/bin/su-exec|usr/bin/python3" | head
```
Expected: the tarball lists `usr/local/bin/su-exec` and `usr/bin/python3` — i.e. it is a complete Debian rootfs with the abox prerequisites. (`bootstrap_vm.sh --profile python-glibc` is exercised end-to-end in Task 6.)

- [ ] **Step 4: Commit**

```bash
git add scripts/bootstrap_vm.sh
git commit -m "build: produce + cache the glibc base tarball in bootstrap_vm.sh"
```

---

## Task 6: `build_rootfs.sh` — shared staging + glibc branch

**Files:** Modify `scripts/build_rootfs.sh`

- [ ] **Step 1: Add `profile_libc()`**

In `scripts/build_rootfs.sh`, after `profile_output_dir()` (~line 49), add:
```bash
profile_libc() {
    case "$ABOX_PROFILE" in
        *-glibc) printf 'glibc\n' ;;
        *)       printf 'musl\n' ;;
    esac
}
```

- [ ] **Step 2: Extract the shared abox staging into a function**

In `inner_mode`, move the steps that are identical across bases — **CLI install, shim+symlinks, init install, ext4 image, `.inputs` stamp** (currently ~lines 204-254) — into a function `stage_abox_into()` that operates on `$stage`/`$out_dir`/`$img`/`$stamp`. Replace those inline lines with a single `stage_abox_into` call. The function body is the existing code verbatim (npm CLI install, CA-bundle build, shim+symlinks, `/sbin/init`, `mkfs.ext4`, stamp) — **except** the CA-bundle step becomes libc-aware (Step 4) and the stamp records extra glibc fields (Step 5).

- [ ] **Step 3: Branch the base prep on libc**

Wrap the existing base-prep block (current `build_rootfs.sh` lines 154-202: stage Alpine miniroot → `apk add` → uv → user creation) in an `if/else` so it becomes the **musl** arm verbatim, and add the **glibc** arm. The result is:
```bash
    if [[ "$(profile_libc)" == "glibc" ]]; then
        local base_tar="$ABOX_VM_DIR/${ABOX_PROFILE}-rootfs.tar.gz"
        ensure_file "$base_tar" "glibc base tarball for $ABOX_PROFILE"
        echo "  staging Debian/glibc base for '$ABOX_PROFILE'..."
        tar -xzf "$base_tar" -C "$stage"
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
    fi
```
This is a pure relocation of the existing musl code into the `else` arm (do not alter it); only the `if`/glibc arm is new. Note the glibc arm creates `/home/abox` via `useradd -m`, so it only needs to add `.claude`.

- [ ] **Step 4: libc-aware CA bundle (inside `stage_abox_into`)**

The existing CA step globs Alpine's `mozilla/*.crt`. Make it libc-aware:
```bash
    echo "  building system CA trust bundle..."
    mkdir -p "$stage/etc/ssl/certs"
    if [[ "$(profile_libc)" == "musl" ]]; then
        if ls "$stage/usr/share/ca-certificates/mozilla/"*.crt >/dev/null 2>&1; then
            cat "$stage/usr/share/ca-certificates/mozilla/"*.crt \
                > "$stage/etc/ssl/certs/ca-certificates.crt"
        fi
    else
        # Debian's ca-certificates package already populated
        # /etc/ssl/certs/ca-certificates.crt; assert it exists.
        [ -s "$stage/etc/ssl/certs/ca-certificates.crt" ] || {
            echo "ERROR: glibc base missing /etc/ssl/certs/ca-certificates.crt" >&2
            exit 1
        }
    fi
```

- [ ] **Step 5: Record glibc provenance in the `.inputs` stamp**

In the stamp block (~lines 241-254), add, for glibc profiles, the base digest, resolved package versions, and tool versions:
```bash
        if [[ "$(profile_libc)" == "glibc" ]]; then
            echo "libc=glibc"
            echo "base_dockerfile=$(sha256sum "$SCRIPT_DIR/glibc/$ABOX_PROFILE.Dockerfile" | cut -d' ' -f1)"
            echo "pkg_versions=$(chroot "$stage" dpkg-query -W -f '${Package}=${Version}\n' 2>/dev/null | sha256sum | cut -d' ' -f1)"
            echo "node_version=$(chroot "$stage" node --version 2>/dev/null)"
            echo "uv_version=$(chroot "$stage" uv --version 2>/dev/null)"
        else
            echo "libc=musl"
        fi
```

- [ ] **Step 6: End-to-end build of the profile**

Run (Docker + builder image required; this exercises Tasks 4–6 together):
```bash
./scripts/bootstrap_vm.sh --yes --profile python-glibc
ls -lh "$HOME/.abox/vm/profiles/python-glibc/rootfs.raw"
cat "$HOME/.abox/vm/profiles/python-glibc/rootfs.raw.inputs"
```
Expected: a `rootfs.raw` is produced under `profiles/python-glibc/`, and the `.inputs` stamp shows `libc=glibc` plus the recorded versions.

- [ ] **Step 7: Confirm the musl path still builds (refactor is behavior-preserving)**

The musl base profile must still build after the refactor (the musl code was only relocated into the `else` arm + shared staging fn). Rebuild it and confirm the staged package set is unchanged via the `.inputs` stamp:
```bash
./scripts/bootstrap_vm.sh --yes
ls -lh "$HOME/.abox/vm/rootfs.raw"
grep -E "^packages=|^libc=" "$HOME/.abox/vm/rootfs.raw.inputs"
```
Expected: the base `rootfs.raw` builds, `.inputs` shows `libc=musl` and the same `packages=` line as before the change. If the package set differs, the relocation altered the musl path — revert and redo it as a pure move.

- [ ] **Step 8: Commit**

```bash
git add scripts/build_rootfs.sh
git commit -m "build(rootfs): glibc branch + shared staging; libc-aware CA + provenance stamp"
```

---

## Task 7: tier-vm proof — manylinux wheels work on `python-glibc`

**Files:** Modify `scripts/local/e2e_test.sh`

- [ ] **Step 1: Add a gated python-glibc assertion**

In `scripts/local/e2e_test.sh`, in the gated VM section (phase 6/7, after the base VM checks), add a block that runs only when the `python-glibc` image exists:
```bash
if [[ -f "$ABOX_VM/profiles/python-glibc/rootfs.raw" ]]; then
    section "phase 6b — python-glibc manylinux wheels (gated)"

    step "python-glibc reports manylinux platform tags (and not musllinux)"
    how 'abox run --task glibc-tags --ephemeral -- /bin/sh -lc "pip debug --verbose"'
    TAGS=$($ABOX run --task glibc-tags --ephemeral --network open --timeout 120 \
        -- /bin/sh -lc 'pip debug --verbose 2>/dev/null' 2>&1 || true)
    if grep -q "manylinux" <<<"$TAGS" && ! grep -q "musllinux" <<<"$TAGS"; then
        pass "python-glibc advertises manylinux tags"
    else
        fail "python-glibc manylinux tags" "expected manylinux and no musllinux"
    fi

    step "python-glibc installs a manylinux wheel and imports it"
    OUT=$($ABOX run --task glibc-wheel --ephemeral --network open --timeout 240 \
        -- /bin/sh -lc 'pip install --quiet --only-binary=:all: cryptography && python3 -c "import cryptography; print(cryptography.__version__)"' 2>&1 || true)
    if grep -qE "^[0-9]+\." <<<"$OUT"; then
        pass "python-glibc installed + imported a manylinux wheel"
    else
        fail "python-glibc manylinux install" "$OUT"
    fi
fi
```
Notes for the implementer:
- `pip debug --verbose` is built into pip; the tag check is the robust, package-independent discriminator.
- `cryptography` ships manylinux wheels; it is the smoke (not the discriminator). If its wheel matrix changes, swap for any current manylinux-only package — the tag check above remains authoritative.
- Network `open` is needed because the wheel download goes through the egress proxy to PyPI; ensure the test config/policy allows `pypi.org`/`files.pythonhosted.org` (the e2e scratch policy may need those egress domains added — confirm and add if the install is blocked).

- [ ] **Step 2: Run the gated assertion (needs KVM + the built image)**

Run:
```bash
RUSTUP_TOOLCHAIN=stable ./scripts/local/e2e_test.sh 2>&1 | grep -A2 "phase 6b"
```
Expected: `python-glibc advertises manylinux tags` and `installed + imported a manylinux wheel` both pass. (If PyPI egress is blocked, add the domains and re-run — see the note above.)

- [ ] **Step 3: Commit**

```bash
git add scripts/local/e2e_test.sh
git commit -m "test(e2e): assert python-glibc resolves manylinux wheels (gated)"
```

---

## Task 8: Documentation

**Files:** Modify `README.md`, `docs/explainer.md`, `templates/config.example.toml`, `docs/future-work.md`, `AGENTS.md`

- [ ] **Step 1: README profile list**

In `README.md`, in the profiles list (the `base`/`node`/`python`/`rust` bullets), add:
```markdown
- `python-glibc` — Python on a Debian/glibc base so `pip`/`uv` install
  `manylinux` wheels (numpy, pandas, scipy, …). Larger image than the musl
  `python` profile; choose it when you need prebuilt scientific wheels.
```

- [ ] **Step 2: explainer.md — the libc axis**

In `docs/explainer.md`, near the guest-rootfs/profiles discussion, add a short subsection explaining: profiles are built on Alpine/musl by default; `pip` on musl only sees `musllinux` wheels (and `gcompat` does not change pip's platform tag); the `python-glibc` profile uses a Debian/glibc base so `manylinux` wheels resolve. Note it's produced via a Docker build + export consumed by the same rootfs builder, and that `su-exec` is provided by `gosu` on the Debian base so the boot path is identical.

- [ ] **Step 3: config example**

In `templates/config.example.toml`, where `environment.profile` is shown, add a comment line listing `python-glibc` as an option with the one-line rationale.

- [ ] **Step 4: future-work.md**

In `docs/future-work.md`, add under an open/roadmap section:
```markdown
- glibc `node-glibc` / `rust-glibc` profiles — now a one Dockerfile + one enum
  arm each, on top of the libc axis added for `python-glibc`.
- Profile naming symmetry: a `python-musl` alias so the libc axis reads
  consistently (without renaming existing profiles).
- `abox init` / `doctor` hint steering data-science users to `python-glibc`.
```

- [ ] **Step 5: AGENTS.md**

In `AGENTS.md`, where the rootfs build is described, note that glibc profiles are produced from `scripts/glibc/<profile>.Dockerfile` via `bootstrap_vm.sh`'s `produce_glibc_base`, and that touching `scripts/glibc/**` or the rootfs build still requires `just rebuild-rootfs` + `just e2e-vm` per the pre-PR checklist.

- [ ] **Step 6: Commit**

```bash
git add README.md docs/explainer.md templates/config.example.toml docs/future-work.md AGENTS.md
git commit -m "docs: document the python-glibc profile and libc axis"
```

---

## Final verification

- [ ] **Rust gate**
```bash
RUSTUP_TOOLCHAIN=stable cargo fmt --all -- --check
RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=stable cargo test --workspace
```
Expected: all clean.

- [ ] **Build + VM gate (needs Docker + KVM)**
  - `./scripts/bootstrap_vm.sh --yes --profile python-glibc` builds the image; `.inputs` shows `libc=glibc`.
  - `./scripts/bootstrap_vm.sh --yes` still builds the unchanged musl base.
  - `just e2e-vm` passes, including the new `phase 6b` manylinux assertions.
  - Add the `vm-attested` label + timestamp/machine comment (the diff touches `crates/abox-core/**` and the rootfs build).

- [ ] **Open the PR** off `feat/glibc-guest-profiles` per the pre-PR checklist (conventional commits, docs updated, `just rebuild-rootfs` run because guest build inputs changed).
