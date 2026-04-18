# abox, explained — every component, top to bottom

This is the long-form companion to [`docs/tutorial.md`](tutorial.md). The tutorial shows you *how* to use abox in 10 minutes; this document explains *why* every piece exists. Target reader: a junior engineer who is comfortable with git and the command line but has never built a microVM, used virtiofs, or written a credential proxy. We will not pretend you are five years old, but we will not assume you've heard of any of this either.

If a section feels too basic, skip it. If a section feels too dense, file an issue and we'll improve it.

---

## 1. The big picture

abox lets you run **multiple AI coding agents in parallel** on the same repository, each in its own isolated sandbox, without giving any of them direct access to your credentials or your filesystem outside the part they're working on.

The whole thing fits in one diagram:

```
┌──────────────────── HOST (your laptop) ────────────────────┐
│                                                            │
│   abox run --task X -- claude                              │
│        │                                                   │
│        ▼                                                   │
│   ┌─────────────────┐                                      │
│   │ Orchestrator    │                                      │
│   │ (abox-core)     │                                      │
│   └─────────────────┘                                      │
│        │                                                   │
│        │ supervises                                        │
│        ▼                                                   │
│   ┌─────────────────────────────────────────┐              │
│   │  Cloud Hypervisor microVM               │              │
│   │   ┌──────────────────────────────┐      │              │
│   │   │ Alpine guest (busybox+socat) │      │              │
│   │   │   ┌──────────┐               │      │              │
│   │   │   │  agent   │  (claude,     │      │              │
│   │   │   │  process │   etc.)       │      │              │
│   │   │   └──┬───────┘               │      │              │
│   │   │      │                       │      │              │
│   │   │   git│ symlink → abox-shim   │      │              │
│   │   │      ▼                       │      │              │
│   │   │   /run/abox-proxy.sock       │      │              │
│   │   └──────┬───────────────────────┘      │              │
│   │          │ socat → vsock CID 2:5000     │              │
│   └──────────┼──────────────────────────────┘              │
│              │                                             │
│              ▼ unix socket                                 │
│   ┌──────────────────┐    ┌────────────────────────┐       │
│   │ proxy bridge     │───▶│ policy engine          │       │
│   │ (per-VM, on host)│    │ allow / deny / inject  │       │
│   └────────┬─────────┘    └────────────────────────┘       │
│            │ exec on host                                  │
│            ▼                                               │
│   ┌──────────────────────────────────────────────┐         │
│   │ real `git push` running with real creds      │         │
│   └──────────────────────────────────────────────┘         │
│                                                            │
│        ┌─────── three virtiofs shares ──────┐              │
│        │  workspace (RW) — git worktree     │              │
│        │  aboxmeta   (RO) — boot metadata   │              │
│        │  aboxstatus (RW) — exit code       │              │
│        └────────────────────────────────────┘              │
└────────────────────────────────────────────────────────────┘
```

The agent runs **inside** a microVM. The two narrow channels between the agent and the outside world are:

1. **The shim/bridge path** for command-line tools (`git`, `gh`, `aws`). The shim is a static-musl binary masquerading as `git` etc. inside the guest. It serializes the command, sends it over a Unix socket → `socat` → `vsock` → host bridge, the bridge consults the policy engine, runs the command on the host with real credentials, and sends back the output.

2. **The egress proxy path** for HTTPS API calls (Anthropic, OpenAI, GitHub, etc.). Today this is a passthrough; future work will turn it into a TLS-terminating MITM that injects API-key headers from host environment variables. See [`docs/plans/2026-04-08-credential-injection.md`](plans/2026-04-08-credential-injection.md).

Everything else — the boot kernel, the rootfs, the virtiofs shares, the bootstrap script — exists to make those two channels work cleanly without the user needing to think about it.

---

## 2. Git worktrees

**What they are:** A git worktree is an additional checked-out copy of a repository that shares the underlying `.git` storage with the original. You get a second working directory on a different branch, but pushes/pulls/branch operations all see the same history.

```bash
cd ~/myrepo                      # branch main
git worktree add ../wt feature   # ../wt is now branch feature, sharing ~/myrepo/.git
```

**Why we use them:** Each agent gets its own sandbox, and each sandbox needs its own working directory because two agents editing the same files at the same time is a recipe for corruption. The naïve solutions are bad:

- **Full clones** waste disk space (every clone duplicates `.git`) and slow down clone time on big repos.
- **Plain branches** force agents to coordinate via stashes / branch switches, which sequentializes them.
- **Overlay filesystems** don't integrate with git's branching model — you can't `git diff` across overlays cleanly.

Worktrees are the right shape: zero-copy isolation at the directory level, full git semantics at the storage level, instant creation. abox creates one worktree per sandbox under `~/.abox/state/worktrees/<task>` on a new branch named `agent/<task>`.

**What would break without them:** Either the agents would step on each other's working directories, or abox would have to copy the whole repo per sandbox (slow, wasteful, and the merge story gets messy because each copy diverges in parallel).

---

## 3. microVMs and why not containers

**What a microVM is:** A real virtual machine — own kernel, own memory, own scheduler — that boots in 100-200 milliseconds and uses ~30 MB of host RAM at idle. The "micro" is about boot time and footprint, not about being a fake VM. Cloud Hypervisor and Firecracker are the two well-known options.

**Why a VM at all:** Because containers share the host kernel. If the agent process inside a Docker container exploits a kernel bug, it can escape to the host. The Linux kernel is huge (~30 million lines of C) and historically has had a steady stream of privilege-escalation bugs. A VM puts a hardware boundary (the CPU's virtualization extensions) between the guest and the host kernel; an agent has to break out of the guest kernel **and** out of the VMM itself before it touches your host. That's a much larger attack surface to defeat.

**Why Cloud Hypervisor specifically over Firecracker:** virtiofs. Firecracker doesn't support virtiofs as of 2026; it offers block devices and 9p instead. We need to mount a git worktree into the guest with full read-write semantics so the agent can edit files normally. virtiofs gives us that with near-native performance; 9p is slow, and block devices would require copying the whole worktree into a disk image. See [ADR-001](decisions/001-architecture.md) for the full rationale.

**What would break without microVMs:** We'd be back to either trusting the agent enough to give it host access (no), or building container-level isolation that has known weaker boundaries. The threat model is "this agent might run untrusted code or be compromised by prompt injection" — we want a real boundary.

---

## 4. virtiofs: how the worktree gets into the guest

**What it is:** virtiofs is a filesystem-sharing protocol designed specifically for VMs. The host runs a `virtiofsd` daemon that exposes a directory; the guest mounts it as `mount -t virtiofs <tag> <mountpoint>`. Operations go over a `vhost-user` shared-memory ring (zero-copy on both sides), so reads and writes have near-native performance.

**How abox uses it:** Each sandbox gets **three** virtiofs shares:

1. `workspace` (read-write) — the git worktree at `/workspace` in the guest. This is what the agent edits.
2. `aboxmeta` (read-write in the FS sense, but mostly read-only in practice) — a small per-sandbox host directory containing `boot.json` (sandbox id, env vars) and `runner.sh` (the literal `exec` line for the agent command). Mounted at `/abox-meta`. Avoids cmdline-length and quoting hell.
3. `aboxstatus` (read-write, single-purpose) — host directory the guest writes its **exit code** into before poweroff. Mounted at `/abox-status`. See [ADR-002](decisions/002-aboxstatus-share.md) for the design.

**Why not 9p?** 9p is slow (no shared memory, every op is a network round-trip). virtiofs is roughly 10x faster on small file operations and is what Cloud Hypervisor recommends.

**Why not a block device?** A block device would require copying the worktree into a disk image at boot and copying it back on shutdown. virtiofs lets the agent's writes land directly in the host worktree, so `abox merge` afterward is just `git merge` with no file copying.

**What would break without it:** Either we'd be copying the worktree on every boot/exit (slow for big repos), or we'd be exposing a network filesystem with much weaker performance.

---

## 5. vsock: how the guest talks to the host without networking

**What vsock is:** A socket address family (AF_VSOCK) for VM-to-host communication. Each VM has a "context ID" (CID); the host is always CID 2. A guest process can `connect(AF_VSOCK, addr={cid:2, port:5000})` and talk to the host directly — no IP networking, no NIC, no routing.

**How abox uses it:** Cloud Hypervisor exposes a per-VM vsock device with `--vsock cid=3,socket=<host-side-path>`. Cloud Hypervisor creates a unix socket on the host at `<path>_5000` for guest connections to vsock port 5000. Inside the guest, `socat` bridges `/run/abox-proxy.sock` ↔ `vsock:2:5000`. The shim connects to the unix socket; socat forwards over vsock; the host's per-VM bridge accepts the connection on the host-side socket.

**Why this is safer than a TCP socket:** No IP stack, no NICs, no routing tables. The guest cannot accidentally talk to anything except CID 2. There is no network the guest can scan, no SSRF vector, no way for the guest to send a misdirected packet to the user's office network. The attack surface is exactly one syscall.

**Why this is the *attribution* boundary:** Every connection that arrives on the per-VM host-side path *provably* came from one specific VM, because Cloud Hypervisor binds the socket and only that VM has access to it. The bridge tags every request with `sandbox_id=Fixed(<task>)` and the audit log records that as ground truth — the guest cannot spoof its own id.

**What would break without it:** We'd need either an IP network to the guest (with all the firewall + routing complexity that entails) or a serial-console-based protocol (which is slow and conflicts with stdout). vsock is the modern, narrow channel.

---

## 6. The shim: `abox-shim`

**What it is:** A small (~1 MB), static, musl-compiled Rust binary, no async runtime, no third-party deps beyond `serde` and `abox-protocol`. Lives at `/usr/local/bin/abox-shim` inside the guest.

**Why it's a symlink:** During rootfs build, `build_rootfs.sh` creates symlinks `/usr/local/bin/git → abox-shim`, `/usr/local/bin/gh → abox-shim`, `/usr/local/bin/aws → abox-shim`. When the agent runs `git push`, the kernel resolves `git` to `abox-shim`, exec's it, and `abox-shim` reads `argv[0]` to figure out which command was invoked.

**What happens when the agent types `git push origin main`:**

1. The kernel exec's `/usr/local/bin/git`, which is a symlink to `abox-shim`.
2. `abox-shim` reads `argv[0]` (`git`) and `argv[1..]` (`push origin main`).
3. Resolves the current working directory:
   - Prefer `ABOX_CWD` (set by `runner.sh` from the host's known truth).
   - Fall back to the target of `/proc/self/cwd` (more reliable than `getcwd(2)` inside virtiofs on some kernels).
   - Fall back to `getcwd(2)`.
   - Fall back to `/workspace`.
4. Reads `ABOX_SANDBOX_ID` from the env (set by guest init from `boot.json`).
5. Builds a `ProxyRequest { command: "git", args: ["push","origin","main"], cwd, sandbox_id }` and serializes it as a JSON line.
6. Connects to `/run/abox-proxy.sock` (the unix socket bridged to vsock).
7. Sends the line, half-closes the write end, reads back a `ProxyResponse { exit_code, stdout, stderr }` JSON line.
8. Prints stdout/stderr, exits with `exit_code`.

**Why it has no async runtime:** Smaller binary (one less crate boundary), faster startup, fewer surprises in the guest where the runtime is minimal. Synchronous code is fine because the shim only ever handles one request per process invocation.

**What would break without it:** The agent would have to know it's inside a sandbox and call abox's API directly. That works for a custom agent, but most third-party tools don't know about abox. The shim makes the sandbox boundary invisible — the agent thinks it's running `git`, and it is, just on the host.

---

## 7. The policy daemon and the per-VM bridge

**What the policy engine is:** A TOML-driven allow/deny matcher for CLI commands and HTTPS destinations. Lives in `crates/abox-core/src/policy.rs`. For a command like `git push origin main`:

1. Strip known git global options off the front (`-c key=val`, `-C path`, `--git-dir`, `--no-pager`, etc.). Reject any unknown global option — this is the fix for [S1](backlog/2026-04-08-vm-e2e-mvp-followups.md).
2. Find the policy entry for the binary (`git`).
3. Check deny patterns first. Anything matching `--force` or `-f\b` or `push\s+--delete` is denied immediately.
4. Check allow patterns. At least one must match (e.g., `^push\s+origin\s+\S+$`).
5. Return `Decision::Allow` or `Decision::Deny(reason)`.

**What the per-VM bridge is:** An embedded copy of the abox-proxyd CLI proxy server that runs *inside* the orchestrator process, not as a separate daemon. One bridge per VM. It binds the host-side path Cloud Hypervisor exposes for vsock port 5000 (`<runtime>/vsock-<id>.sock_5000`), accepts connections from the guest's socat bridge, parses incoming `ProxyRequest` lines, evaluates them against the policy engine, executes allowed commands on the host with real credentials, and writes the `ProxyResponse` back.

**The walkthrough for `git push origin main`:** Picking up where the shim section left off:

1. `git push origin main` is serialized into `{command:"git",args:["push","origin","main"],cwd:"/workspace",sandbox_id:"my-task"}` and sent over vsock to the bridge.
2. The bridge looks at `cwd_map`: if the request CWD starts with `/workspace`, rewrite it to the host worktree path (so `git` runs on the right files, not on a non-existent guest path).
3. Determine the sandbox_id from `SandboxAttribution::Fixed("my-task")` — the bridge **ignores** the request's own `sandbox_id` field because the per-VM socket already proves provenance.
4. Call `policy.evaluate_cli("git", ["push","origin","main"])`. Returns `Allow`.
5. Build a `tokio::process::Command::new("git")` with the rewritten CWD, the host's PATH, and either pass through or remove `SSH_AUTH_SOCK` based on `policy.forward_ssh_agent("git")`.
6. Run the command, capture stdout + stderr + exit code.
7. Write to the audit log: `{"sandbox_id":"my-task","command":"git","args":["push","origin","main"],"decision":"allowed","exit_code":0,"timestamp":...}`.
8. Send `ProxyResponse { exit_code, stdout, stderr }` back to the shim.

**Why the bridge runs in the orchestrator and not in `abox-proxyd`:** Two reasons. First, the orchestrator owns the VM lifecycle, so it can bind the per-VM socket *before* CH boots — eliminating any race window. Second, attribution: the per-VM socket guarantees provenance, so the orchestrator's bridge uses `SandboxAttribution::Fixed` and the audit log is unambiguous. The standalone `abox-proxyd` still works for users who want a system daemon, but it uses `SandboxAttribution::FromRequest` and falls back to `"unknown"` for legacy clients.

**What would break without the policy engine:** The agent would have direct host shell access. Any prompt-injection attack could end with `rm -rf ~`. The policy engine is the chokepoint that says "the agent can run `git push origin <branch>` but not `git push --force` and not `aws iam delete-user`".

---

## 8. The HTTPS egress proxy (TLS-terminating MITM)

**What it does:** The egress proxy (`abox-proxyd::egress_proxy`) intercepts outbound HTTPS traffic from the guest and injects API credentials into requests, so secrets never enter the VM. See [ADR-003](decisions/003-https-credential-injection.md) for the full design rationale.

**How it works:**

1. The guest sets `HTTPS_PROXY=http://127.0.0.1:18443` (injected automatically by the orchestrator). `127.0.0.1:18443` is a `socat` bridge inside the guest that forwards to the host over vsock port 5001. When the agent's HTTPS client sends a CONNECT request, it arrives at the host-side proxy.
2. The proxy evaluates the target domain against egress policy rules. If denied, it returns 403.
3. If the domain is in the `bypass_tls` list (for cert-pinned clients), the proxy does a plain TCP passthrough — no TLS termination.
4. Otherwise, the proxy generates a per-host leaf certificate signed by the abox root CA (`~/.abox/ca/root.crt`), sends `200 Connection Established`, and accepts the client's TLS using that leaf cert. The guest trusts it because the root CA was baked into the rootfs at build time.
5. The proxy reads the plaintext HTTP request, injects the credential header (e.g., `x-api-key: <value>` for Anthropic, `Authorization: Bearer <value>` for OpenAI), then opens a new TLS connection to the real upstream using system root certificates.
6. The modified request is forwarded upstream and the response is relayed back to the client.

**The credential mapping** is defined in `policies/default.toml`. Credentials can come from a host environment variable or from a JSON credential file on the host:

```toml
# API key from host environment variable
[[egress]]
domain = "api.anthropic.com"
inject_header = "x-api-key"
env_var = "ANTHROPIC_API_KEY"
header_template = "{value}"

# OAuth token from a JSON credential file on the host
[[egress]]
domain = "api.claude.ai"
inject_header = "Authorization"
credential_file = "~/.claude/.credentials.json"
json_path = "claudeAiOauth.accessToken"
header_template = "Bearer {value}"
```

The `env_var` and `credential_file` fields are both read from the **host**, not the guest. The `json_path` field is a dot-separated path into the JSON credential file (e.g., `claudeAiOauth.accessToken`). The `header_template` supports `{value}` substitution.

**Stub credential files:** Some tools (e.g., Claude Code) check for a local credential file before making any network calls and refuse to start if the file is absent. To satisfy this check without placing real credentials in the VM, configure a `stub` in `~/.abox/config.toml`:

```toml
[guest]
[[guest.credential_files]]
host = "~/.claude/.credentials.json"
guest = "~/.claude/.credentials.json"

[guest.credential_files.stub.claudeAiOauth]
accessToken = "abox-proxy-managed"
expiresAt = 9999999999999
refreshToken = "abox-proxy-managed"
```

The stub is written into the guest filesystem at boot with placeholder values. The real token is injected by the proxy at the network layer; the stub value never reaches the upstream API.

**Node.js CA trust:** Node.js does not use the system trust store by default. The orchestrator injects `NODE_EXTRA_CA_CERTS` pointing to the abox root CA so that Node.js-based tools (Claude Code, Codex CLI) trust the MITM certificate automatically.

**What this means for you:** Set API keys as host environment variables (`export ANTHROPIC_API_KEY=sk-...`) for key-based APIs. For OAuth-based tools, configure `credential_file` in the egress policy and a `stub` in the guest config. The proxy injects the real credentials; the agent never sees them.

**CA management:** The root CA is generated on first use and lives at `~/.abox/ca/`. Use `abox ca show` to inspect it, `abox ca rotate` to regenerate (triggers a rootfs rebuild), and `abox ca path` to find it.

---

## 9. The orchestrator: `abox-core::sandbox`

**What it is:** The state machine that owns the lifecycle of a sandbox. Lives in `crates/abox-core/src/sandbox.rs`. The two main methods are:

- **`create_sandbox(params)`** — creates a git worktree on `agent/<task>`, builds a `VmConfig`, calls `vm_manager.start(config)`. If VM start fails, rolls back the worktree.
- **`run_sandbox(params, policy)`** — calls `create_sandbox`, then spawns a per-VM proxy bridge bound to vsock-5000, spawns a console streamer that tails the CH console log to stdout, polls `vm_manager.info()` until the VM exits (or `--timeout` fires), drains the console pump, reads the guest exit code from `aboxstatus/exit-code`, and returns it. If `--timeout` was specified and the VM exceeds it, the orchestrator attempts graceful shutdown, waits a 10-second grace period, force-kills if needed, and returns exit code 124. If `--ephemeral` was set, the worktree and branch are cleaned up regardless of exit code. If the guest never wrote an exit code (catastrophic VM failure before init.sh ran), it logs a warning to both tracing and stderr, rolls the worktree back, and returns 1.

**State machine diagram (rough):**

```
                     create_sandbox          run_sandbox loop
   pending  ──────▶  worktree+vm  ──────▶   running  ──────▶  exited
                          │                                       │
                          │ (start fails)                         │
                          ▼                                       │
                       rolled-back ◀──── (no exit code) ──────────┘
```

**Why it polls instead of waiting:** The `VmPort` trait doesn't expose a "wait for exit" primitive. Adding one would couple every adapter to a particular waiting mechanism. Polling `info()` every 250 ms is a small overhead and works for every adapter (real CH or in-memory mock). The poll interval is now centralized in `VmRuntimeTuning` so tests can tighten it.

**What would break without the orchestrator:** Each CLI command would have to assemble `virtiofsd + cloud-hypervisor + the bridge + the console streamer + the cleanup` itself. The orchestrator is the single place that knows the full lifecycle.

---

## 10. The bootstrap script: `bootstrap_vm.sh`

**What it is:** A bash script that downloads pinned + checksummed copies of `cloud-hypervisor`, `ch-remote`, `virtiofsd`, the Linux kernel (`vmlinux`), and the Alpine miniroot, then builds the static-musl shim and assembles an ext4 rootfs containing busybox + socat + bash + Node.js + the shim + Claude Code + Codex CLIs + a guest init script. The rootfs also includes an unprivileged `abox` user (uid=1000) and `su-exec` for privilege dropping — the agent command runs as this user, not root (see ADR-004). CLI versions are pinned in `build_rootfs.sh` for reproducible builds. Supports both x86_64 and aarch64 hosts (auto-detected via `uname -m`). Also supports `--from-bundle <path>` to restore from a pre-built tarball (published alongside GitHub Releases) instead of downloading individual components.

**Why it's bash and not Rust:** Bootstrapping is a one-time operation per machine. Bash is universal, easy to read, and avoids the chicken-and-egg problem of "you need cargo to build the bootstrap, but the bootstrap installs cargo's musl target". The `vendor/` cache and SHA256 checksums make it idempotent and recoverable from network blips. The 200 lines are ~50% comments and pinned-version constants — the actual logic is small.

**What gets checksummed:**

| Artifact | SHA256 pinned in script |
|---|---|
| cloud-hypervisor v44.0 | yes |
| ch-remote v44.0 | yes |
| virtiofsd v1.10.0 (extracted from Ubuntu .deb) | yes (twice — deb and binary) |
| vmlinux (CH-built kernel) | yes |
| alpine-minirootfs-3.19.9.tar.gz | yes |
| socat-1.8.0.0-r0.apk | yes |

If any of these get re-published or corrupted in transit, the bootstrap fails fast with a clear "expected vs actual" message and asks you to delete the cached file under `vendor/`.

**Why it doesn't need sudo:** Every step runs in user space:
- Downloads land in `~/.abox/vm/`.
- The deb extraction uses `dpkg-deb -x` (no install).
- The shim builds with cargo.
- The rootfs is assembled in a `mktemp -d` staging dir and packed into an ext4 image with `mkfs.ext4 -d` (which writes a populated filesystem from a directory tree, no loop mount needed).
- Symlinks land in `~/.local/bin`.

**What would break without it:** Users would have to install Cloud Hypervisor manually (it's not packaged in most distros), find a kernel that boots without an initramfs, build virtiofsd from source, and assemble a rootfs by hand. That's a 2-hour task at best and a "give up" task at worst.

---

## 11. The end-to-end test: `scripts/e2e_test.sh`

**What it is:** A seven-phase bash script (`./scripts/e2e_test.sh` or `just e2e`) that exercises every major component without needing a CI runner with KVM enabled.

**The seven phases:**

1. **build** — `cargo build --workspace`. Catches compile errors before anything else.
2. **unit + integration tests** — `cargo test --workspace`. Catches test regressions.
3. **scratch git repo + abox config** — Sets up a self-contained test environment under `.scratch/e2e-run-<pid>`.
4. **abox CLI workspace ops** — Tests `abox list`, the rollback path when VM start fails, simulated worktrees, `divergence`, `merge`, `stop --clean`.
5. **abox-proxyd CLI policy enforcement** — Starts the standalone proxy daemon, sends allowed and denied requests to it, verifies the audit log attribution and the legacy-shim fallback.
6. **full VM end-to-end** *(gated)* — Boots a real microVM, runs `git status` inside it, verifies audit log attribution, tests `--detach` lifecycle, and asserts exit code propagation. Skipped if `~/.abox/vm/` artifacts aren't present (so phases 1-5 work in CI).
7. **agent lifecycle** *(gated)* — Full agent commit/diverge/deny/merge cycle: boots a sandbox that creates a file and commits, verifies divergence reporting, tests policy denial of `git push --force`, merges the work into main, and cleans up. Also runs the HTTPS credential injection e2e (gated on `ANTHROPIC_API_KEY`).

**How to add an eighth phase:** Append a `section "phase 8 — ..."` block at the end of `scripts/e2e_test.sh`. Use `step` / `how` / `expect` / `pass` / `fail` for each assertion. The summary footer counts every `pass`/`fail` invocation, so new phases are picked up automatically.

**Why the e2e is in bash and not Rust:** Because phases 4-6 test the *binary* (`./target/debug/abox`) and its actual filesystem and process side effects, not its library API. Bash + the abox CLI is the most accurate simulation of how a user invokes it. The Rust unit tests in `cargo test --workspace` cover the library-level cases (mocks, bypass parsers, exit code helpers); the e2e covers the integration cases.

---

## 12. Putting it all together: the lifecycle of one `abox run`

Let's walk through what happens when you press Enter on:

```bash
abox run --task fix-auth -- claude
```

**Stage 1: CLI parsing.** clap parses args, builds `RunArgs { task: "fix-auth", ..., command: ["claude"] }`. `commands::run::execute` is called.

**Stage 2: Orchestrator setup.** `Cli::parse` already loaded the config and built a `SandboxOrchestrator<Git2Workspace, CloudHypervisorAdapter>`. The policy engine was loaded from `~/.abox/policies/default.toml`.

**Stage 3: Worktree creation.** `orchestrator.run_sandbox(params, policy)` calls `create_sandbox`, which calls `workspace.create_worktree("fix-auth", "main")`. `Git2Workspace` runs `git worktree add ~/.abox/state/worktrees/fix-auth -b agent/fix-auth main`.

**Stage 4: VM config.** A `VmConfig` is built with the worktree path, the configured kernel and rootfs, memory, vcpus, env vars, and `agent_command = ["claude"]`.

**Stage 5: VM start.** `vm_manager.start(vm_config)` invokes `CloudHypervisorAdapter::start`:
1. Allocates short-suffixed socket paths under `<runtime>/`: `vfs-fix-auth.sock` (workspace), `vfs-meta-fix-auth.sock` (meta), `vfs-status-fix-auth.sock` (status), `ch-api-fix-auth.sock` (CH API), `vsock-fix-auth.sock` (vsock).
2. Stages `<runtime>/meta-fix-auth/boot.json` + `runner.sh` containing the agent command and env.
3. Pre-creates `<runtime>/status-fix-auth/exit-code` (empty).
4. Spawns three `virtiofsd` processes — one per share — and waits for each socket to appear.
5. Spawns `cloud-hypervisor` with `--memory shared=on`, three `--fs` entries, `--vsock cid=3,...`, `--console file=...`, the kernel, and the rootfs.
6. Waits for the CH API socket to appear, returns a `VmInfo`.

**Stage 6: Bridge + console streamer.** Back in `run_sandbox`, the orchestrator:
1. Spawns a `ProxyBridge` on `<runtime>/vsock-fix-auth.sock_5000` with `SandboxAttribution::Fixed("fix-auth")` and a CWD map `/workspace → <real worktree>`.
2. Spawns a console tailer on `<runtime>/console-fix-auth.log` with a shutdown `Notify`.

**Stage 7: Guest boot.** The Linux kernel boots in ~150 ms, mounts `/workspace`, `/abox-meta`, and `/abox-status` from virtiofs, runs `/sbin/init` (which is the embedded `guest/init.sh`):
1. Mount `/proc`, `/sys`, `/dev`.
2. Print "abox guest init: online".
3. `socat UNIX-LISTEN:/run/abox-proxy.sock,fork VSOCK-CONNECT:2:5000 &` — starts the unix↔vsock bridge.
4. `if sh /abox-meta/runner.sh; then RC=0; else RC=$?; fi`
5. Inside `runner.sh` (runs as root initially):
   - Pre-flight: `getent passwd abox` — exits 69 if the rootfs is missing the unprivileged user.
   - Stages credential stubs from `/abox-meta/credentials/` into `/home/abox/.claude/` (or `.codex/`), chowns them to `abox:abox`.
   - Fixes `/home/abox` ownership via `chown -R abox:abox /home/abox`.
   - Drops privileges: `exec su-exec abox:abox env HOME=/home/abox USER=abox claude …`
6. The agent runs as `uid=1000(abox)`, not root. The workspace virtiofs share is launched with `--uid-map=:1000:<host_uid>:1:` so host-owned worktree files appear as uid 1000 in the guest and agent-created files land on the host owned by the host user. See ADR-004.
7. Eventually the agent exits with some code N.
8. `echo $N > /abox-status/exit-code; sync`
9. `kill $SOCAT_PID; poweroff -f`

**Stage 8: Each guest `git`/`gh`/`aws` invocation** (while the agent is running) goes through the shim → vsock → bridge → policy → exec → audit → response cycle described in section 7. Console output goes through the kernel's serial driver → `--console file=...` → console tailer → orchestrator's stdout.

**Stage 9: VM exit.** Cloud Hypervisor's `--cmdline` had `quiet`, but the kernel still prints the poweroff message. `ch_child.try_wait()` in the orchestrator's poll loop sees the process is gone, calls `cleanup_vm_files(id, vm, remove_status_dir=false)` — this cleans the sockets but **leaves the status dir** so `run_sandbox` can read the exit code. The polling loop's `info()` returns Err, the loop breaks.

**Stage 10: Cleanup + return.** The bridge is aborted. The console shutdown is signalled; the tailer drains to EOF and exits within 500 ms. `read_exit_code(<runtime>/status-fix-auth/exit-code)` reads "0" (or whatever). The status dir is removed. `run_sandbox` returns `Ok(0)`.

**Stage 11: CLI exit.** `commands::run::execute` checks the exit code:
- `0` → prints "Sandbox 'fix-auth' exited cleanly." → `Ok(())` → process exits 0.
- `≠ 0` → prints "Sandbox 'fix-auth' exited with code N." → `std::process::exit(N)` so the OS sees the actual code (not anyhow's generic 1).

**Stage 12: Worktree preserved.** The git worktree at `~/.abox/state/worktrees/fix-auth` is still there. The user can `cd` into it, inspect what claude did, run `abox merge fix-auth` to integrate, or `abox stop fix-auth --clean` to throw it away.

That's the whole lifecycle. Every step has a single owner and a clear failure mode.

---

## Where to go next

- **[`docs/tutorial.md`](tutorial.md)** — actually do all of this on your machine in 10 minutes.
- **[`docs/decisions/`](decisions/)** — the architecture decision records for the choices that shaped abox.
- **[`docs/plans/`](plans/)** — historical and planned implementation work.
- **[`scripts/e2e_test.sh`](../scripts/e2e_test.sh)** — the canonical "is this thing working?" gate (46 assertions across 7 phases).
- **[`docs/future-work.md`](future-work.md)** — what's next after the priorities roadmap landed.
- **[`docs/decisions/003-https-credential-injection.md`](decisions/003-https-credential-injection.md)** — ADR for the TLS-terminating MITM proxy architecture.
