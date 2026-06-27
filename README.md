# abox — Parallel AI Agent Sandboxing

`abox` is a lightweight, secure tool for running multiple AI coding agents in parallel, isolated sandboxes. It combines **git worktrees** with **microVMs** (Cloud Hypervisor) to provide agents with independent workspaces, while securely proxying credentials via a dual-layer interception architecture.

## Why `abox`?

When running multiple autonomous agents on a single codebase, you face three problems:
1. **Workspace collisions:** Agents stepping on each other's git branches and files.
2. **Credential leaks:** Giving agents direct access to your AWS or GitHub tokens is dangerous.
3. **Host system risk:** Agents running `rm -rf /` or installing malware.

`abox` solves this by:
- Isolating each agent in a fast-booting **Cloud Hypervisor microVM**.
- Mounting independent **git worktrees** into the VM via `virtiofs`.
- Proxying commands and HTTP requests out of the VM through a **strict, TOML-configured policy engine**.
- Injecting API credentials into outbound HTTPS requests via a **TLS-terminating MITM proxy**, so secrets never enter the VM.

## Architecture

`abox` is built in Rust using a Hexagonal (Ports & Adapters) architecture.

1. **`abox-core`**: Domain logic (Workspace manager, VM lifecycle, Policy engine).
2. **`abox-cli`**: The user interface (CLI commands and TUI dashboard).
3. **`abox-proxyd`**: The host-side daemon that evaluates policies and executes allowed commands.
4. **`abox-shim`**: A static musl binary injected into the guest VM that intercepts commands (via symlinks) and forwards them to `proxyd`.

![Architecture](.plans/architecture-diagram.png)

## Getting Started

### Prerequisites

- Linux host with `/dev/kvm` accessible to your user
- Rust toolchain (`cargo`)
- `just` command runner (`cargo install just`)

### Installation

> **Note:** `v0.3.2` is released, but source install remains the recommended
> path while the installer and release packaging continue to harden.

**From source** (recommended):

```bash
# Prerequisites: Rust (https://rustup.rs), just (cargo install just)
git clone https://github.com/X-McKay/abox.git
cd abox
cargo build --release

# Add the compiled binary to your PATH (or copy it to ~/.local/bin)
export PATH="$PWD/target/release:$PATH"

abox init             # guided first-run setup: downloads VM stack,
                      # writes config, installs default policy

# Optionally install additional official guest profiles up front
abox init --profile node --profile python --profile rust
```

Or run the steps individually:

```bash
just bootstrap-vm     # downloads the VMM, kernel, builds the rootfs,
                      # and symlinks the binaries into ~/.local/bin
abox doctor           # verify the environment before first use
```

The guest rootfs is a 768 MiB sparse Alpine image that includes bash,
Node.js/npm, Python 3, `su-exec`, the system CA bundle, `abox-shim`, and
pinned Claude Code / Codex CLIs.

Official guest profiles are also available for repo-owned workflows:

- `base`
- `node`
- `python`
- `python-glibc` — Python on a Debian/glibc base so `pip`/`uv` install
  `manylinux` wheels (numpy, pandas, scipy, …). Larger image than the musl
  `python` profile; choose it when you need prebuilt scientific wheels.
- `rust`

These install as separate rootfs images under `~/.abox/vm/profiles/` and are
selected by repo config, not by raw image path.

`virtiofsd` also needs `cap_sys_admin+ep` on the installed runtime binary
before the first sandbox boot. `abox init` now checks that directly, and
the bootstrap/install scripts print the exact `sudo setcap ...` command
when it is still missing.

`bootstrap-vm` is idempotent and uses checksummed cached downloads, so
re-running it is fast (seconds, not minutes). Currently supports **x86_64**
hosts only — aarch64 support is in progress. See
[`docs/vm-setup.md`](docs/vm-setup.md) for the full setup walkthrough.

**From release artifacts** (optional):

```bash
curl -fsSL https://raw.githubusercontent.com/X-McKay/abox/main/scripts/install.sh | bash
```

### Documentation

- [`docs/tutorial.md`](docs/tutorial.md) — 10-minute walkthrough from
  `git clone` to your first sandbox
- [`docs/explainer.md`](docs/explainer.md) — architecture deep dive:
  what every component does and why
- [`docs/vm-setup.md`](docs/vm-setup.md) — VM stack installation +
  troubleshooting
- [`docs/audit-log.md`](docs/audit-log.md) — audit log format, verification,
  and tamper-evidence threat model
- [`docs/decisions/`](docs/decisions/) — architecture decision records
- [`docs/future-work.md`](docs/future-work.md) — forward-looking
  roadmap; what's next and why

### Configuration

The easiest way to configure abox is to run `abox init`, which writes
`~/.abox/config.toml` with all paths pre-filled and installs the default
policy automatically.

To configure manually:

```bash
mkdir -p ~/.abox/policies
cp templates/config.example.toml ~/.abox/config.toml
cp policies/default.toml ~/.abox/policies/default.toml
# Then edit ~/.abox/config.toml to set image_path and kernel_path
# to the output of 'just bootstrap-vm' (~/.abox/vm/rootfs.raw and
# ~/.abox/vm/vmlinux).
```

By default, abox stores all state under `~/.abox/` (worktrees, templates,
logs, and the runtime socket directory). No root access required.

Run `abox doctor` at any time to check your environment for common setup
problems, including the `virtiofsd` file capability required for its
namespace sandbox.

### Usage

1. **Probe the machine-readable capability envelope:**
   ```bash
   abox --capabilities
   ```
   Prints a JSON envelope describing supported protocol versions, task
   kinds, and execution engines. This bypasses config/policy loading so
   external harnesses can probe abox before first-run setup.

2. **Start an agent sandbox:**
   ```bash
   abox run --task fix-auth --base main -- claude
   ```

3. **Set up a repo-owned workflow with network intent and a guest profile:**
   ```bash
   abox project init --profile node

   cat > .abox/project.toml <<'EOF'
   [network]
   mode = "scoped"
   bundles = ["npm-public"]

   [environment]
   profile = "node"
   caches = ["npm"]
   prepare = ".abox/prepare.sh"
   EOF

   cat > .abox/prepare.sh <<'EOF'
   #!/bin/sh
   set -e
   npm ci --ignore-scripts --no-fund --no-audit
   EOF
   chmod +x .abox/prepare.sh

   abox project validate
   abox project trust
   abox env warm
   ```

4. **Launch a known managed agent with a prompt file:**
   ```bash
   abox run --task fix-auth --prompt-file prompts/fix-auth.md -- codex
   ```

5. **Start with runtime controls:**
   ```bash
   abox run --task fix-auth --timeout 300 --ephemeral -- claude
   # --timeout N: kill after N seconds (exit code 124)
   # --ephemeral: auto-remove sandbox after exit
   ```

6. **Override the repo's network mode for a single run:**
   ```bash
   abox run --task docs-scan --network open --prompt-file prompts/research.md -- claude
   ```

7. **Fast start from a template (snapshot restore, ~100ms):**
   ```bash
   abox template create --name base --from running-sandbox
   abox run --template base --task fix-auth -- claude
   ```

8. **List running sandboxes:**
   ```bash
   abox list
   ```

9. **Check divergence across agents:**
   ```bash
   abox divergence
   ```

10. **Merge a completed task:**
   ```bash
   abox merge fix-auth
   ```

11. **Manage the CA (for HTTPS credential injection):**
   ```bash
   abox ca show      # fingerprint + expiry
   abox ca rotate    # regenerate CA + rebuild rootfs
   abox ca path      # print CA directory
   ```

12. **Enable managed auth providers (Claude Code, Codex):**
   ```bash
   # Edit ~/.abox/config.toml and add:
   # [auth.providers.claude]
   # enabled = true
   #
   # [auth.providers.codex]
   # enabled = true
   #
   # See docs/explainer.md Section 8 and docs/credential-scoping.md.
   ```

13. **Grant transparent credential injection for a service:**
   ```bash
   abox grant providers                 # list built-in shortcuts
   abox grant add openai                # inject $OPENAI_API_KEY into api.openai.com
   abox grant add my-svc --domain api.my.com --header Authorization --env MY_TOKEN
   abox grant list                      # show configured grants (incl. path rules)
   abox grant remove openai
   ```
   The agent only ever sees a placeholder — the real token is injected by the
   host proxy into outbound HTTPS and never enters the VM.

14. **Authorize an MCP server over OAuth (PKCE + state, refresh supported):**
   ```bash
   abox grant mcp auth https://mcp.example.com --client-id <id> --scopes "read write"
   abox grant mcp list
   abox grant mcp refresh example-com    # use the stored refresh token
   abox grant mcp remove example-com
   ```
   Tokens are stored under `~/.abox/mcp-tokens/<name>.json` with `0600`
   permissions.

15. **Ephemeral service sidecars (Postgres/Redis/Ollama/MySQL):**
   ```toml
   # .abox/project.toml
   [services]
   postgres = { version = "17" }
   redis = { version = "7" }
   ollama = { models = ["qwen2.5-coder:7b"] }
   ```
   ```bash
   abox services available              # list supported services
   abox services show                   # show this repo's configured services
   abox run --task feat -- claude       # starts services, injects ABOX_*_URL,
                                        # bridges them into the guest, tears down on exit
   ```
   Requires Docker on the host. The connection URL is injected as an env var
   (e.g. `ABOX_POSTGRES_URL`) reachable from inside the guest.

16. **Snapshot a running sandbox and restore it later:**
   ```bash
   abox snapshot create --name wip --from fix-auth
   abox snapshot list                   # names, sizes, total disk usage
   abox snapshot restore wip --as fix-auth-2
   abox snapshot prune --keep 5         # delete oldest, keep 5 most recent
   abox snapshot delete wip
   ```

17. **Inspect and verify the tamper-evident audit log:**
   ```bash
   abox audit show -n 50                # recent proxied CLI/egress requests
   abox audit show --sandbox fix-auth --request-type egress
   abox audit verify                    # check the keyed hash chain + tip
   ```
   The chain is HMAC-keyed with a host-only key (`~/.abox/logs/audit.key`,
   `0600`) so a sandboxed agent cannot forge it, and truncation is detected via
   a persisted chain tip. See `docs/audit-log.md` for the threat model.

For profile-backed repo environments:

- `node` is validated for `npm`
- `python` is validated for `uv` / `pip3`, and prepare flows should prefer
  `uv`-managed virtual environments over `uv pip install --system`
- `python-glibc` is the same as `python` but runs on a Debian/glibc base;
  use it when your prepare flow installs packages that only ship `manylinux`
  wheels (numpy, pandas, scipy, etc.)
- `rust` is validated for `cargo`, but the current guest toolchain is
  `rustc/cargo 1.76.0`; repos requiring Cargo edition 2024 or Cargo.lock v4
  need a newer guest toolchain before warming

## Development

We use `just` as our command runner. Install it with `cargo install just`.

- `just check`: Run formatting, lints, and tests.
- `just lint`: Run clippy with strict warnings.
- `just build-shim`: Build the guest shim (requires the musl target for your host architecture).

See [CONTRIBUTING.md](CONTRIBUTING.md) for detailed development guidelines.

## Performance

Measured on x86_64, 32 cores, kernel 6.14.0-37-generic. VM benchmarks averaged over 5 runs.
Updated at release v0.6.0 (2026-06-27).

| Metric | Value | What it measures |
|---|---|---|
| VM boot | 160 ms | Cloud Hypervisor start to first proxied request |
| Proxy round-trip | 160 ms | Bridge ready to `git status` response |
| Full `abox run` | 314 ms | Total wall time for trivial guest command |
| Sandbox cleanup | 14 ms | `abox stop --clean` teardown |
| Policy evaluation | ~45.236 ns | `evaluate_cli` for `git status` (allowed) |
| Request serialization | ~45.972 ns | JSON encode of `ProxyRequest` |
| Boot meta generation | ~177.48 ns | `BootMeta::to_json()` |
| Release binary | 12.1 MB | `target/release/abox` (LTO + strip) |

Run `just bench` (criterion, no VM) or `just bench-vm-n 5` (VM latency) to reproduce.

## License

Apache 2.0
