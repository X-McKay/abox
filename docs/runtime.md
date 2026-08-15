# The MicroSandbox runtime

Since [ADR-008](decisions/008-microsandbox-runtime-and-product-boundary.md),
abox runs each sandbox as a MicroSandbox microVM. MicroSandbox is built on
libkrun — KVM on Linux, Hypervisor.framework on macOS Apple Silicon — and
provides the generic isolation substrate (VMM, kernel, OCI image handling,
filesystem brokering, resource limits). abox owns everything above it: what
the agent is authorized to do.

This page covers how the runtime works, what it needs from the host, and how
to troubleshoot it.

## Architecture

The orchestrator resolves task intent into a runtime-neutral
`SandboxRuntimeSpec` (workspace mount, environment profile, resources, staged
inputs, control channels, network plan, lifecycle) and hands it to the
MicroSandbox adapter (`crates/abox-core/src/adapters/microsandbox.rs`). The
adapter translates it mechanically and never widens it.

### Guest images

Environment profiles (`base`, `node`, `python`, `python-glibc`, `rust`)
resolve to pinned OCI images through the manifest embedded in the abox binary
(`images/manifest.toml`). Images are pulled on first use. See
[Guest images and the manifest](#guest-images-and-the-manifest) below.

### Workspace mount and ownership overlay

The task worktree bind-mounts read-write at `/workspace`. Guest ownership is
granted through the mount's metadata overlay: a root `chown` inside the guest
before the agent starts makes the workspace appear owned by the guest `abox`
user (uid 1000), while host inodes keep their real owner. Files the agent
creates land on the host owned by the host user. `mount_excludes` become tmpfs volumes shadowing
workspace subdirectories.

### Guest staging via rootfs patches

Prompt, prepare-script, credential-stub, and input files are staged as
pre-boot rootfs patches under `/abox-meta` (read-only modes). The shim's
transport declaration (`/etc/abox/transport` → `vsock:5000`) is host-staged
and immutable — the guest cannot redirect its own broker traffic. Host-staged
guest binaries from `~/.abox/guest/<arch>/` are patched into every image at
start, keeping the shim protocol in lockstep with the host binary even when
the OCI image bakes an older copy.

### Control channels

Each control channel maps a well-known guest vsock port to a per-sandbox host
Unix socket:

- **Command broker (guest port 5000):** the `git`/`gh`/`aws` shim connects to
  `/run/abox-proxy.sock` inside the guest; the persistent `abox-bridge`
  process forwards it over one long-lived vsock uplink to the host
  `CommandBroker`, which evaluates policy, executes allowed commands with
  real credentials, and audits. Retry semantics are phase-aware: only connect/send
  failures retry — a lost response never re-executes a privileged command.
- **HTTPS egress (guest port 5001):** the agent's `HTTPS_PROXY` points at
  guest loopback `127.0.0.1:18443`, bridged by `abox-bridge` to the host
  egress proxy (TLS-terminating MITM with credential injection).
- **Service bridges (guest ports 51xx):** declared sidecar/host-port
  services get loopback listeners bridged the same way.

Attribution is host-route-based: connections arriving on a per-sandbox host
socket provably came from that sandbox. The guest never asserts its own
identity.

The persistent `abox-bridge` uplink exists to work around two quirks in the
current MicroSandbox VMM's vsock routing: half-close is not propagated, and
rapid per-process reconnects to a recently used port are reset. A long-lived
process's connections are reliable, so the broker multiplexes all shim
invocations over one held connection. See
[`runtime-upgrades.md`](runtime-upgrades.md) for the regression checks these
quirks imply.

### Exec model and exit codes

The agent runs as a non-root exec (default uid 1000, the `abox` user baked
into the guest images) through the runtime's guest agent, so its exit code
propagates directly — there is no status-share file protocol. Agent launch is
deferred until the orchestrator has bound the broker/egress listeners, and
agent exit stops the sandbox. There is no guest init script: the runtime
supplies PID 1 and user switching.

### Network plans

abox's `safe`/`scoped`/`open` modes compile in abox policy
(`compile_runtime_network_plan`) to one of two plans:

- **HostMediated** (`safe`): guest networking fully disabled; all egress
  rides the vsock control channels.
- **Native** (`scoped`/`open`): compiled allow-lists enforced by the runtime
  (DNS-pinned, SNI-verified, TCP 443 + DNS only). Host, loopback, private
  ranges, link-local, and cloud metadata are always denied — `open` is
  public-internet only, never unrestricted.

See [`security-model.md`](security-model.md) for the invariants.

### Fast, repeatable environments

There is no memory-snapshot or template mechanism. `abox env warm` runs the
repo's prepare flow once inside a real guest and persists durable caches, so
subsequent sandboxes start against a warm environment.

## Host requirements

- **Linux (x86_64 or aarch64):** `/dev/kvm` accessible to your user
  (`sudo usermod -aG kvm $USER`, then log out/in).
- **macOS (Apple Silicon):** Hypervisor.framework (checked via
  `sysctl kern.hv_support`). Intel Macs are not supported.
- The MicroSandbox runtime assets: the `msb` binary and libkrunfw guest
  firmware. `abox init` installs them via the SDK; `abox doctor` verifies
  them.

There is no kernel download, no rootfs build, and no `setcap` step —
`abox init` installs everything the runtime needs.

### `MSB_HOME`

The runtime assets live under `$MSB_HOME`, default `~/.microsandbox`:

```text
~/.microsandbox/
  bin/msb           # the MicroSandbox runtime binary
  lib/libkrunfw*    # guest firmware
```

Set `MSB_HOME` to relocate them; abox resolves it the same way the SDK does.
MicroSandbox also keeps its own image cache and sandbox state under this
directory, so the first sandbox per profile pulls the guest image into it.

### Host-staged guest binaries

`abox init` stages the release's static musl guest binaries under
`~/.abox/guest/<arch>/` (`abox-shim`, `abox-bridge`; the guest architecture
always matches the host architecture). If they are absent, `abox doctor`
warns but sandboxes still work — the official images bake fallback copies.
Developers build them locally with `just build-guest-bins`.

## Guest images and the manifest

`images/manifest.toml` is the host-owned map from profile names to pinned OCI
image references (`ghcr.io/x-mckay/abox-guest-*`). It is embedded in the abox
binary at compile time; repos select a *profile*, never an image reference.

- `digest = "sha256:…"` pins exact image content; the publish workflow
  (`.github/workflows/images.yml`) fills digests in after each build. An
  empty digest means "not yet published": abox resolves the profile by tag
  and `abox doctor` reports it as unpinned. The 0.7 image series is
  published to `ghcr.io/x-mckay` with all profile digests pinned.
- `[images.overrides]` in `~/.abox/config.toml` is a development escape
  hatch mapping a profile to an arbitrary reference (or a local rootfs path,
  used verbatim). Host-owned config only — repo config can never choose an
  image. `abox doctor` flags overridden profiles.

See [`images/README.md`](../images/README.md) for the image contract,
multi-arch requirements, and local build instructions.

## Running an agent

```bash
# Inside a git repo you want the agent to work on:
abox run --task fix-auth --base main -- claude
```

`abox run` starts a fresh microVM with your git worktree mounted at
`/workspace`, exec's `claude` inside it as the unprivileged guest user, and
returns the agent's exit code directly. The agent's `git`/`gh`/`aws` calls
are routed through the host policy broker and attributed to the sandbox id in
the audit log at `~/.abox/logs/audit.jsonl`.

## Verifying the install

```bash
abox doctor         # checks virtualization, msb assets, guest binaries,
                    # image-manifest resolution, CA, policy, audit log
just e2e-runtime    # live end-to-end suite against real microVMs
```

`just e2e-runtime` (`scripts/local/msb_e2e_test.sh`) exercises run/exit-code
propagation, workspace write-through and isolation, the command broker
(proxied git, policy deny, audit attribution), and the host-mediated HTTPS
egress path. It skips cleanly (exit 0) when the msb runtime assets or
virtualization are missing.

## Customizing

Resource defaults live in `~/.abox/config.toml`:

```toml
[sandbox_defaults]
memory_mib = 2048
vcpus      = 2
```

## Troubleshooting

**`abox doctor` reports "msb not found" or missing libkrunfw**
The MicroSandbox runtime assets are not installed. Run `abox init` — it
downloads them into `$MSB_HOME` (default `~/.microsandbox`) via the SDK. If
you installed `msb` yourself somewhere else, either put it on `PATH` or set
`MSB_HOME` to its install root.

**Unix socket path too long**
Control channels are per-sandbox host Unix sockets under `runtime_dir`
(default `~/.abox/r`), named `msb-<id>.sock_<port>`. abox budgets against the
104-byte cap (the macOS floor; Linux allows 108), so a deeply nested
`runtime_dir` or a very long task id can push past the limit. Keep
`runtime_dir` short and check the worst case with `abox doctor`. On macOS,
`/tmp` is a symlink (`/private/tmp`); abox canonicalizes bind paths, but if
you point config at symlinked paths yourself, prefer the canonical form.

**First run of a profile is slow**
The first sandbox per profile pulls its OCI image from `ghcr.io` into the
MicroSandbox image cache — expect a one-time download of a few hundred MB
(more for `python-glibc`). Subsequent runs reuse the cache. `abox init`
prints the image each requested profile will pull; pre-pull by running a
trivial sandbox per profile, or warm the environment with `abox env warm`.

**Profile fails to resolve / "no guest image mapping"**
The embedded manifest in your abox build has no entry for the profile.
Upgrade abox, or add a temporary `[images.overrides]` entry in
`~/.abox/config.toml`.

**`abox doctor` warns about unpinned digests**
The manifest entry for the profile has no content digest — the image either
has not been published for this release yet or you are on an override. Runs
still work (tag-addressed), but content is not pinned; treat this as a
development-only state.

**Hardware virtualization unavailable**
On Linux, check `/dev/kvm` exists and is accessible (`ls -la /dev/kvm`;
`sudo usermod -aG kvm $USER` then re-login). On macOS, `sysctl
kern.hv_support` must print `1` — this requires Apple Silicon; nested
virtualization inside another VM generally does not work.
