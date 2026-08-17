# Guest profiles

Every abox sandbox starts from an official, digest-pinned OCI image. A
repository selects a profile in `.abox/project.toml`; it cannot select an
arbitrary image reference or weaken the host isolation boundary.

Run `abox project init` for a safe starter config. It prints a recommendation
from common repository metadata without writing the choice automatically. Make
the final choice explicitly, then review and trust the resulting repo config.

```bash
abox project init
abox project set-profile python-glibc
abox project trust
```

## `base`

Use the default profile for shell-oriented tasks or projects that do not need a
language-specific toolchain.

## `node`

Use for projects with `package.json`. Configure durable caches and a
guest-native prepare command when dependency installation is repeated:

```toml
[environment]
profile = "node"
caches = ["npm"]
prepare = ".abox/prepare.sh"
watch = ["package-lock.json"]
```

## `python`

Use for ordinary Python projects. The image is musl-based; prefer `uv` with a
virtual environment in the prepare flow rather than a system install.

## `python-glibc`

Use for Python projects that depend on packages with manylinux wheels, such as
numpy, pandas, scipy, torch, or pyarrow. It uses a Debian/glibc base and is
larger than `python`, but avoids musllinux wheel-resolution failures.

```toml
[environment]
profile = "python-glibc"
caches = ["pip", "uv"]
prepare = ".abox/prepare.sh"
watch = ["pyproject.toml", "uv.lock"]
```

## `rust`

Use for Cargo projects. Add `cargo` to the durable cache list when a repeated
prepare flow builds dependencies. Check the image's documented toolchain
version before selecting language features or lockfile formats that need a
newer compiler.

## Common problems

- `externally-managed-environment` in Python: create a virtual environment
  and let `uv` or `pip` install there.
- Python wheels cannot be resolved: switch from `python` to `python-glibc`.
- First run is slow: the selected image is pulled once into MicroSandbox's
  image cache; later runs reuse it.

See [`runtime.md`](runtime.md) for image and host-runtime troubleshooting.
