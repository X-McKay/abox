# abox, explained — every component, top to bottom

This is the long-form companion to [`docs/tutorial.md`](tutorial.md). The tutorial shows you *how* to use abox in 10 minutes; this document explains *why* every piece exists. Target reader: a junior engineer who is comfortable with git and the command line but has never booted a microVM or written a credential proxy. We will not pretend you are five years old, but we will not assume you've heard of any of this either.

If a section feels too basic, skip it. If a section feels too dense, file an issue and we'll improve it.

abox's isolation runtime is **MicroSandbox** (libkrun: KVM on Linux,
Hypervisor.framework on macOS Apple Silicon) — see [`runtime.md`](runtime.md)
for the runtime's host requirements and troubleshooting, and
[ADR-008](decisions/008-microsandbox-runtime-and-product-boundary.md) for the
runtime/product boundary.

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
│   │  MicroSandbox microVM (libkrun)         │              │
│   │   ┌──────────────────────────────┐      │              │
│   │   │ OCI guest image (abox user)  │      │              │
│   │   │   ┌──────────┐               │      │              │
│   │   │   │  agent   │  (claude,     │      │              │
│   │   │   │  process │   etc.)       │      │              │
│   │   │   └──┬───────┘               │      │              │
│   │   │      │                       │      │              │
│   │   │   git│ symlink → abox-shim   │      │              │
│   │   │      ▼                       │      │              │
│   │   │   /run/abox-proxy.sock       │      │              │
│   │   └──────┬───────────────────────┘      │              │
│   │          │ abox-bridge → vsock 5000     │              │
│   └──────────┼──────────────────────────────┘              │
│              │                                             │
│              ▼ per-sandbox unix socket                     │
│   ┌──────────────────┐    ┌────────────────────────┐       │
│   │ command broker   │───▶│ policy engine          │       │
│   │ (per-sandbox)    │    │ allow / deny / inject  │       │
│   └────────┬─────────┘    └────────────────────────┘       │
│            │ exec on host                                  │
│            ▼                                               │
│   ┌──────────────────────────────────────────────┐         │
│   │ real `git push` running with real creds      │         │
│   └──────────────────────────────────────────────┘         │
│                                                            │
│        the task's git worktree bind-mounts read-write      │
│        at /workspace inside the guest                      │
└────────────────────────────────────────────────────────────┘
```

The agent runs **inside** a microVM. The two narrow channels between the agent and the outside world are:

1. **The shim/broker path** for command-line tools (`git`, `gh`, and other intercepted CLIs). The shim is a static-musl binary masquerading as `git` etc. inside the guest. It serializes the command and sends it over a guest Unix socket; the persistent `abox-bridge` process forwards it over vsock to the host's per-sandbox command broker, which consults the policy engine, runs the command on the host with real credentials, and sends back the output.

2. **The egress proxy path** for HTTPS API calls (Anthropic, OpenAI, GitHub, etc.). A TLS-terminating MITM proxy that injects host-held credentials into allowed requests — see section 8.

Everything else — the runtime, the guest images, the file staging — exists to make those two channels work cleanly without the user needing to think about it.

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

**What a microVM is:** A real virtual machine — own kernel, own memory, own scheduler — that boots in well under a second and has a small idle footprint. The "micro" is about boot time and footprint, not about being a fake VM. libkrun (which MicroSandbox builds on) and Firecracker are well-known implementations.

**Why a VM at all:** Because containers share the host kernel. If the agent process inside a Docker container exploits a kernel bug, it can escape to the host. The Linux kernel is huge (~30 million lines of C) and historically has had a steady stream of privilege-escalation bugs. A VM puts a hardware boundary (the CPU's virtualization extensions) between the guest and the host kernel; an agent has to break out of the guest kernel **and** out of the VMM itself before it touches your host. That's a much larger attack surface to defeat.

**Why MicroSandbox specifically:** abox delegates the generic isolation substrate — VMM, guest kernel, OCI image handling, mounts, resource limits — to a purpose-built runtime instead of maintaining its own hypervisor integration. MicroSandbox (built on libkrun) gives abox everything the sandbox boundary needs: hardware virtualization on both Linux (KVM) and macOS Apple Silicon (Hypervisor.framework), a read-write bind mount of the git worktree, per-sandbox vsock routes for the brokered channels, and OCI images as the guest-environment format. It is pinned to an exact version and upgraded only through qualified PRs because it sits inside the trusted computing base ([`runtime-upgrades.md`](runtime-upgrades.md)). See [ADR-001](decisions/001-architecture.md) for the original architecture rationale and [ADR-008](decisions/008-microsandbox-runtime-and-product-boundary.md) for the runtime decision.

**What would break without microVMs:** We'd be back to either trusting the agent enough to give it host access (no), or building container-level isolation that has known weaker boundaries. The threat model is "this agent might run untrusted code or be compromised by prompt injection" — we want a real boundary.

---

## 4. The workspace mount: how the worktree gets into the guest

**How it works:** The task's git worktree bind-mounts **read-write at `/workspace`** inside the guest. The runtime provides the shared-filesystem mechanics; abox declares the mount in the runtime spec and never copies the worktree.

**The ownership overlay:** Host worktree files are owned by your host user, but the agent runs as the guest `abox` user (uid 1000). Guest ownership is granted through the mount's metadata overlay — a root `chown` inside the guest before the agent starts makes `/workspace` appear owned by uid 1000, while the host inodes keep their real owner. Files the agent creates land on the host owned by the host user, so `abox merge` afterward is just `git merge` with no file copying.

**Staged inputs:** Prompt files, the repo's prepare script, credential stubs, and `--input-file` payloads are staged as **pre-boot rootfs patches** under `/abox-meta` with read-only modes — they are part of the guest filesystem before the agent ever runs, so the guest cannot swap them out. `mount_excludes` from the repo config become tmpfs volumes shadowing workspace subdirectories (e.g. to keep a giant `node_modules` out of the shared mount).

**Why not copy the worktree in?** Copying into a disk image at boot and back out on shutdown is slow for big repos and makes the merge story messy (which copy wins?). The bind mount lets the agent's writes land directly in the host worktree while everything else in the guest stays ephemeral.

**What would break without it:** Either we'd be copying the worktree on every boot/exit, or the agent's work would be trapped inside the guest when the sandbox stops.

---

## 5. vsock: how the guest talks to the host without networking

**What vsock is:** A socket address family (AF_VSOCK) for VM-to-host communication. Each VM has a "context ID" (CID); the host is always CID 2. A guest process can `connect(AF_VSOCK, addr={cid:2, port:5000})` and talk to the host directly — no IP networking, no NIC, no routing.

**How abox uses it:** The runtime maps each well-known guest vsock port to a **per-sandbox host Unix socket** under `runtime_dir` (named `msb-<id>.sock_<port>`, e.g. `msb-fix-auth.sock_5000` for the command broker). Inside the guest, the persistent `abox-bridge` process listens on `/run/abox-proxy.sock` and forwards over one long-lived vsock uplink to port 5000; the host's per-sandbox command broker accepts the connection on the host-side socket. The same pattern carries HTTPS egress (guest loopback `127.0.0.1:18443` → vsock 5001) and declared service bridges (guest ports 51xx).

**Why this is safer than a TCP socket:** No IP stack, no NICs, no routing tables. The guest cannot accidentally talk to anything except CID 2. There is no network the guest can scan, no SSRF vector, no way for the guest to send a misdirected packet to the user's office network. The attack surface is exactly one syscall.

> **Exception — declared host-port bridges.** In `scoped`/`open` mode a repo may
> declare `[[host_ports]]` in `.abox/project.toml` to splice a specific guest
> loopback port to an existing host loopback service. This is the one
> operator-authorized, version-controlled exception to "no unmediated outbound":
> it is refused in `safe` mode and every connection is recorded in the audit log
> (`host-port-bridge` at setup, `host-port-connect` per connection). Prefer the
> egress proxy + a `scoped` egress rule whenever the service is reachable over
> the network; the bridge exists for loopback-only host services.

**Why this is the *attribution* boundary:** Every connection that arrives on the per-sandbox host-side socket *provably* came from one specific sandbox, because the runtime binds the socket and only that sandbox's vsock route reaches it. The broker tags every request with `sandbox_id=Fixed(<task>)` and the audit log records that as ground truth — the guest cannot spoof its own id.

**What would break without it:** We'd need either an IP network to the guest (with all the firewall + routing complexity that entails) or a serial-console-based protocol (which is slow and conflicts with stdout). vsock is the modern, narrow channel.

---

## 6. The shim: `abox-shim`

**What it is:** A small (~1 MB), static, musl-compiled Rust binary, no async runtime, no heavy dependencies. Lives at `/usr/local/bin/abox-shim` inside the guest. The official guest images bake it in, and the adapter patches the host-staged copy (from `~/.abox/guest/<arch>/`) into every guest at start so the shim protocol stays in lockstep with the host binary.

**Why it's a symlink:** The guest images ship `/usr/local/bin/git → abox-shim`, `gh → abox-shim`, and `aws → abox-shim` — real `git` is never installed in the guest. When the agent runs `git push`, the kernel resolves `git` to `abox-shim`, exec's it, and `abox-shim` reads `argv[0]` to figure out which command was invoked.

**How it knows where to send requests:** The transport declaration at `/etc/abox/transport` is host-staged into the guest rootfs before boot and immutable to the guest. The default is the guest Unix socket `/run/abox-proxy.sock`, served by the persistent `abox-bridge` process, which multiplexes every exchange over one long-lived vsock uplink to the host command broker. A guest that edits its own transport gains nothing — an unrouted vsock port reaches nothing.

**What happens when the agent types `git push origin main`:**

1. The kernel exec's `/usr/local/bin/git`, which is a symlink to `abox-shim`.
2. `abox-shim` reads `argv[0]` (`git`) and `argv[1..]` (`push origin main`).
3. Resolves the current working directory:
   - Prefer `ABOX_CWD` (host-known truth, set in the agent's environment).
   - Fall back to the target of `/proc/self/cwd` (more reliable than `getcwd(2)` inside shared mounts on some kernels).
   - Fall back to `getcwd(2)`.
   - Fall back to `/workspace`.
4. Reads `ABOX_SANDBOX_ID` from the env (informational only — the host attributes by socket route, not by this value).
5. Builds a `ProxyRequest { command: "git", args: ["push","origin","main"], cwd, sandbox_id }` and serializes it as a JSON line.
6. Connects to the declared transport (`/run/abox-proxy.sock` by default).
7. Sends the line, reads back a `ProxyResponse { exit_code, stdout, stderr }` JSON line. Retry semantics are phase-aware: only connect/send failures retry — a lost response never re-executes a privileged command.
8. Prints stdout/stderr, exits with `exit_code`.

**Why it has no async runtime:** Smaller binary (one less crate boundary), faster startup, fewer surprises in the guest where the runtime is minimal. Synchronous code is fine because the shim only ever handles one request per process invocation.

**What would break without it:** The agent would have to know it's inside a sandbox and call abox's API directly. That works for a custom agent, but most third-party tools don't know about abox. The shim makes the sandbox boundary invisible — the agent thinks it's running `git`, and it is, just on the host.

---

## 7. The policy engine and the per-sandbox command broker

**What the policy engine is:** A TOML-driven allow/deny matcher for CLI commands and HTTPS destinations. Lives in `crates/abox-core/src/policy.rs`. For a command like `git push origin main`:

1. Strip known git global options off the front (`-c key=val`, `-C path`, `--git-dir`, `--no-pager`, etc.). Reject any unknown global option — this is the fix for [S1](backlog/2026-04-08-vm-e2e-mvp-followups.md).
2. Find the policy entry for the binary (`git`).
3. Check deny patterns first. Anything matching `--force` or `-f\b` or `push\s+--delete` is denied immediately.
4. Check allow patterns. At least one must match (e.g., `^push\s+origin\s+\S+$`).
5. Return `Decision::Allow` or `Decision::Deny(reason)`.

**What the command broker is:** `CommandBroker` (`crates/abox-core/src/command_broker.rs`) — the CLI proxy server, run *inside* the orchestrator process, one per sandbox. It binds the per-sandbox host socket the runtime routes for vsock port 5000 (`<runtime>/msb-<id>.sock_5000`), accepts the guest's `abox-bridge` uplink, parses incoming `ProxyRequest` lines, evaluates them against the policy engine, executes allowed commands on the host with real credentials, and writes the `ProxyResponse` back.

**The walkthrough for `git push origin main`:** Picking up where the shim section left off:

1. `git push origin main` is serialized into `{command:"git",args:["push","origin","main"],cwd:"/workspace",sandbox_id:"my-task"}` and sent over vsock to the broker.
2. The broker looks at `cwd_map`: if the request CWD starts with `/workspace`, rewrite it to the host worktree path (so `git` runs on the right files, not on a non-existent guest path).
3. Determine the sandbox_id from `SandboxAttribution::Fixed("my-task")` — the broker **ignores** the request's own `sandbox_id` field because the per-sandbox socket already proves provenance.
4. Call `policy.evaluate_cli("git", ["push","origin","main"])`. Returns `Allow`.
5. Build a `tokio::process::Command::new("git")` with the rewritten CWD, the host's PATH, and either pass through or remove `SSH_AUTH_SOCK` based on `policy.forward_ssh_agent("git")`.
6. Run the command, capture stdout + stderr + exit code.
7. Write to the audit log: `{"sandbox_id":"my-task","command":"git","args":["push","origin","main"],"decision":"allowed","exit_code":0,"timestamp":...}`.
8. Send `ProxyResponse { exit_code, stdout, stderr }` back to the shim.

**Why the broker runs in the orchestrator and not in `abox-proxyd`:** Two reasons. First, the orchestrator owns the sandbox lifecycle, so it can bind the per-sandbox socket *before* the agent launches — eliminating any race window (agent launch is deliberately deferred until the broker and egress listeners are bound). Second, attribution: the per-sandbox socket guarantees provenance, so the orchestrator's broker uses `SandboxAttribution::Fixed` and the audit log is unambiguous. The standalone `abox-proxyd` still works for users who want a system daemon, but it uses `SandboxAttribution::FromRequest`.

**What would break without the policy engine:** The agent would have direct host shell access. Any prompt-injection attack could end with `rm -rf ~`. The policy engine is the chokepoint that says "the agent can run `git push origin <branch>` but not `git push --force` and not `gh repo delete`".

---

## 8. The HTTPS egress proxy (TLS-terminating MITM)

**What it does:** The egress proxy (the request broker — `abox_core::request_broker`, served per-sandbox by the orchestrator and standalone by `abox-proxyd`) intercepts outbound HTTPS traffic from the guest and injects API credentials into requests, so secrets never enter the VM. See [ADR-003](decisions/003-https-credential-injection.md) for the full design rationale.

**How repo-local network intent fits in now:** `abox` can also resolve a
repo-local `.abox/project.toml` into one of three user-facing network modes:

- `safe` — only the host-managed surface
- `scoped` — `safe` plus approved bundles / exact hostnames
- `open` — broad proxy-mediated HTTPS access

These modes do **not** bypass the host mediation boundary. They compile into
the same policy / transport machinery described below:

- managed destinations still use MITM when host-held credentials must be
  applied
- public package registries and other unmanaged destinations use passthrough by
  default
- the guest still has no direct NIC-based outbound path

> **Exception — declared host-port bridges.** In `scoped`/`open` mode a repo may
> declare `[[host_ports]]` in `.abox/project.toml` to splice a specific guest
> loopback port to an existing host loopback service. This is the one
> operator-authorized, version-controlled exception to "no unmediated outbound":
> it is refused in `safe` mode and every connection is recorded in the audit log
> (`host-port-bridge` at setup, `host-port-connect` per connection). Prefer the
> egress proxy + a `scoped` egress rule whenever the service is reachable over
> the network; the bridge exists for loopback-only host services.

**How it works:**

1. The guest sets `HTTPS_PROXY=http://127.0.0.1:18443` (injected automatically by the orchestrator). `127.0.0.1:18443` is an `abox-bridge` listener inside the guest that forwards to the host over vsock port 5001. When the agent's HTTPS client sends a CONNECT request, it arrives at the host-side proxy.
2. The proxy evaluates the target domain against egress policy rules. If denied, it returns 403.
3. If the domain is in the `bypass_tls` list (for cert-pinned clients), the proxy does a plain TCP passthrough — no TLS termination.
4. Otherwise, the proxy generates a per-host leaf certificate signed by the abox root CA (`~/.abox/ca/root.crt`), sends `200 Connection Established`, and accepts the client's TLS using that leaf cert. The guest trusts it because the root CA is staged into the guest trust store at launch.
5. The proxy reads the plaintext HTTP request, injects the credential header (e.g., `x-api-key: <value>` for Anthropic, `Authorization: Bearer <value>` for OpenAI), then opens a new TLS connection to the real upstream using system root certificates.
6. The modified request is forwarded upstream and the response is relayed back to the client.

**The credential mapping** is defined in `policies/default.toml`. For the
default managed providers, credentials come from host-side JSON files or host
environment variables:

```toml
# Claude Code OAuth token from a JSON credential file on the host
[[egress]]
domain = "api.anthropic.com"
inject_header = "Authorization"
credential_file = "~/.claude/.credentials.json"
json_path = "claudeAiOauth.accessToken"
header_template = "Bearer {value}"

# OpenAI-compatible HTTP auth from a host environment variable
[[egress]]
domain = "api.openai.com"
inject_header = "Authorization"
env_var = "OPENAI_API_KEY"
header_template = "Bearer {value}"
```

The `env_var` and `credential_file` fields are both read from the **host**, not
the guest. The `json_path` field is a dot-separated path into the JSON
credential file (e.g., `claudeAiOauth.accessToken`). The `header_template`
supports `{value}` substitution.

**Managed provider stubs:** Some tools (e.g., Claude Code and Codex) check for a
local credential file before making any network calls and refuse to start if
the file is absent. To satisfy that check without placing real credentials in
the VM, enable the managed provider in `~/.abox/config.toml`:

```toml
[auth.providers.claude]
enabled = true

[auth.providers.codex]
enabled = true
```

abox stages the provider's stub into the guest filesystem before boot with
placeholder values. The real token is injected by the proxy at the network
layer; the stub value never reaches the upstream API.

**Node.js CA trust:** Node.js does not use the system trust store by default. The orchestrator injects `NODE_EXTRA_CA_CERTS` pointing to the abox root CA so that Node.js-based tools (Claude Code, Codex CLI) trust the MITM certificate automatically.

**What this means for you:** enable managed providers for Claude Code and/or
Codex, keep their real auth files on the host, and let abox stage the stubs
automatically. GitHub stays host-side through managed `git` / `gh` execution.
The proxy injects the real credentials; the agent never sees them.

**CA management:** The root CA is generated on first use and lives at `~/.abox/ca/`. Use `abox ca show` to inspect it, `abox ca rotate` to regenerate (takes effect on the next sandbox start, since the CA is staged at launch), and `abox ca path` to find it.

---

## 9. The orchestrator: `abox-core::sandbox`

**What it is:** The state machine that owns the lifecycle of a sandbox. Lives in `crates/abox-core/src/sandbox.rs`. The two main methods are:

- **`create_sandbox(params)`** — creates a git worktree on `agent/<task>`, resolves task intent into a runtime-neutral `SandboxRuntimeSpec` (workspace mount, guest profile image, resources, staged inputs, control channels, network plan), and starts the sandbox through the runtime adapter. If start fails, rolls back the worktree.
- **`run_sandbox(params, policy, root_ca)`** — calls `create_sandbox`, binds the per-sandbox `CommandBroker` and egress proxy to the runtime's control sockets (agent output streams through the runtime to this process), waits for the agent to exit, and tears everything down, returning the agent's exit code directly. If `--timeout` was specified and the sandbox exceeds it, the orchestrator attempts graceful shutdown, waits a grace period, force-kills if needed, and returns exit code 124. If `--ephemeral` was set, the worktree and branch are cleaned up regardless of exit code.

**State machine diagram (rough):**

```
                     create_sandbox          run_sandbox
   pending  ──────▶  worktree+vm  ──────▶   running  ──────▶  exited
                          │
                          │ (start fails)
                          ▼
                       rolled-back
```

**What would break without the orchestrator:** Each CLI command would have to assemble the runtime spec, the broker, the egress proxy, the service bridges, and the cleanup itself. The orchestrator is the single place that knows the full lifecycle.

**What it owns beyond raw sandbox startup:** the orchestrator also resolves
repo-owned behavior before launch:

- loads `.abox/project.toml` if present
- applies trust-on-first-use for repo-widened behavior
- stages immutable prompt / prepare inputs as pre-boot rootfs patches
- mounts durable cache roots when configured
- selects the official guest profile image (`base`, `node`, `python`,
  `python-glibc`, `rust`) for the repo
- refreshes guest-native warm state before launch when the repo is configured
  with caches plus a prepare flow

---

## 10. Guest environments: OCI images and `abox init`

There is no VM bootstrap step. `abox init` installs the MicroSandbox runtime
assets (the `msb` binary + libkrunfw guest firmware) under `$MSB_HOME` via
the SDK, and guest environments are **OCI images** pulled on first use.

Each profile (`base`, `node`, `python`, `python-glibc`, `rust`) is a Docker
build context under [`images/`](../images/), published as
`ghcr.io/x-mckay/abox-guest-*` for both amd64 and arm64. Every image ships
the common guest contract: the agent CLIs (Claude Code, Codex) pinned to the
same versions across profiles, an unprivileged `abox` user (uid 1000) the
agent runs as, a `/workspace` mount point, the guest-side abox binaries, and
`git`/`gh`/`aws` as shim symlinks — real `git` is never installed in the
guest. Profiles resolve to digest-pinned references through the manifest
embedded in the abox binary (`images/manifest.toml`); repos select a
*profile*, never an image URL. See [`images/README.md`](../images/README.md)
for the image contract and [`runtime.md`](runtime.md) for host requirements.

---

## 10a. The libc axis: musl vs. glibc guest profiles

All standard profiles (`base`, `node`, `python`, `rust`) are built on **Alpine Linux**, which uses **musl libc**. Alpine is small and fast, but musl has one practical consequence for Python workflows: `pip` and `uv` detect the guest as a `musllinux` platform and will only install `musllinux` wheels. Most scientific Python packages (numpy, pandas, scipy, and the wider PyData stack) only publish `manylinux` wheels — wheels built against glibc — and do not publish `musllinux` variants. This means `pip install numpy` inside the `python` profile either pulls a source distribution (slow, requires a C compiler in the guest) or fails entirely. Installing `gcompat` on musl does **not** fix this: `gcompat` provides a glibc-compatible runtime shim, but `pip`'s platform tag is determined at package-resolution time from the OS ABI, not from what runtime libraries are installed. The platform tag stays `musllinux` and `manylinux`-only packages remain unavailable.

The **`python-glibc`** profile solves this by replacing the Alpine base with **Debian bookworm-slim** (pinned by digest in `images/python-glibc/Dockerfile`). In a glibc guest the platform tag becomes `manylinux`, so prebuilt wheels resolve normally. The profile follows the same guest contract as every other image — same pinned agent CLIs, same `abox` user, same shim symlinks — so there is no special casing for the libc flavor at runtime.

Select the `python-glibc` profile via your repo config:

```toml
# .abox/project.toml
[environment]
profile = "python-glibc"
```

It is an opt-in profile because the Debian base image is larger than the Alpine one; repos that do not need `manylinux` wheels should stay on the default `python` profile.

---

## 11. The end-to-end test: `scripts/local/msb_e2e_test.sh`

**What it is:** The live runtime suite (`just e2e-runtime`). It boots **real MicroSandbox microVMs** and exercises the full substrate end to end — 49 assertions across six phases. It skips cleanly (exit 0) when hardware virtualization or the msb runtime assets under `$MSB_HOME` are missing, so it is safe to invoke anywhere; a skip is not an attestation.

**The phases:**

0. **build + scratch layout** — builds the workspace and the static musl guest binaries, and sets up a self-contained scratch state dir (deliberately short, because Unix socket paths are capped at 104 bytes).
1. **substrate** — boot, exit-code propagation, workspace write-through, isolation between sandboxes, ephemeral cleanup, timeouts.
2. **command broker** — proxied `git` through the shim, policy deny, audit attribution.
3. **HTTPS egress proxy** — policy-enforced CONNECT: managed domains allowed, unmanaged denied, denials stable under repeated attempts. (The SSRF/network-plan invariants on the compiled policy are unit release gates in `just tier-ci`.)
4. **hygiene** — control-socket cleanup, `abox list`, `stop --clean`.
5. **filesystem adversarial** — escape attempts against the workspace boundary.

**How to add an assertion:** Append a `section "..."` block with `step` / `how` / `expect` / `pass` / `fail` calls. The summary footer counts every `pass`/`fail` invocation, so new phases are picked up automatically.

**Why the e2e is in bash and not Rust:** Because it tests the *binary* (`abox`) and its actual filesystem and process side effects, not its library API. Bash + the abox CLI is the most accurate simulation of how a user invokes it. The Rust unit tests in `cargo test --workspace` cover the library-level cases (the shared `MockRuntime`, policy compilation, exit code helpers); the e2e covers the integration cases against real microVMs.

---

## 12. Putting it all together: the lifecycle of one `abox run`

Let's walk through what happens when you press Enter on:

```bash
abox run --task fix-auth --prompt-file prompts/fix-auth.md -- codex
```

**Stage 1: CLI parsing.** clap parses args, builds `RunArgs { task: "fix-auth", ..., command: ["codex"] }`. `commands::run::execute` is called.

**Stage 2: Repo behavior resolution.** If the repo has `.abox/project.toml`, the CLI resolves:

- default network mode (`safe` / `scoped` / `open`)
- optional official guest profile
- durable cache config
- immutable `prepare.sh`
- default prompt file, overridden here by `--prompt-file`

If the repo widens behavior beyond the builtin defaults, `abox` checks the
trust-on-first-use approval fingerprint before continuing.

**Stage 3: Orchestrator setup.** `Cli::parse` already loaded the host config and built a `SandboxOrchestrator<Git2Workspace, MicrosandboxRuntime>`. The host policy engine was loaded from `~/.abox/policies/default.toml`, then narrowed or widened by the resolved repo network mode (compiled into a `RuntimeNetworkPlan` by `compile_runtime_network_plan`).

**Stage 4: Warm-state refresh (optional).** If the repo config defines durable
caches plus a prepare flow, `abox run` checks the recorded warm-state
fingerprint. If it is missing or stale, `abox` launches an ephemeral warm
sandbox first, runs the staged prepare script inside the real guest, persists
cache state, and only then continues to the actual agent run.

**Stage 5: Worktree creation.** `orchestrator.run_sandbox(params, policy, root_ca)` calls `create_sandbox`, which calls `workspace.create_worktree("fix-auth", "main")`. `Git2Workspace` runs `git worktree add ~/.abox/state/worktrees/fix-auth -b agent/fix-auth main`.

**Stage 6: The runtime spec.** Task intent is resolved into a `SandboxRuntimeSpec`: the worktree as a read-write `/workspace` mount, the profile's pinned OCI image, memory/vcpus, env vars, staged inputs (prompt, prepare script, credential stubs, the CA certificate, the transport declaration), the control channels (broker, egress, service bridges), the compiled network plan, and `agent_command = ["codex"]` adapted for prompt-file delivery if needed.

**Stage 7: Sandbox start.** The MicroSandbox adapter translates the spec mechanically:
1. Resolves the profile to its pinned image (pulled into the runtime's cache on first use).
2. Applies the pre-boot rootfs patches (staged files under `/abox-meta`, `/etc/abox/transport`, host-staged `abox-shim`/`abox-bridge` binaries).
3. Declares the vsock routes: guest port 5000 → `<runtime>/msb-fix-auth.sock_5000`, guest port 5001 → `<runtime>/msb-fix-auth.sock_5001`, plus one per declared service bridge.
4. Starts the microVM. The agent is **not** launched yet — launch is deferred so the orchestrator can bind the host-side listeners first.

**Stage 8: Brokers come up.** Back in `run_sandbox`, the orchestrator:
1. Binds the `CommandBroker` on `<runtime>/msb-fix-auth.sock_5000` with `SandboxAttribution::Fixed("fix-auth")` and a CWD map `/workspace → <real worktree>`, wired to the same audit JSONL file `abox-proxyd` uses.
2. Binds the per-sandbox egress proxy on `<runtime>/msb-fix-auth.sock_5001`.

**Stage 9: Agent launch.** With the listeners bound, the runtime's guest agent exec's the agent command as the unprivileged `abox` user (uid 1000) with `HOME=/home/abox`, `HTTPS_PROXY` pointed at guest loopback 18443, and `NODE_EXTRA_CA_CERTS` pointed at the staged root CA. A root `chown` of `/workspace` (the ownership overlay) has already run. The persistent `abox-bridge` process serves `/run/abox-proxy.sock` and the loopback egress port, multiplexing both over vsock.

**Stage 10: Each guest intercepted CLI invocation** (while the agent is running) goes through the shim → `abox-bridge` → vsock → command broker → policy → exec → audit → response cycle described in section 7. Agent stdout/stderr streams through the runtime to the orchestrator's terminal.

**Stage 11: Agent exit.** The agent exits with some code N. Because it runs as a direct exec through the runtime's guest agent, N propagates directly — no status file, no exit-code protocol. Agent exit stops the sandbox.

**Stage 12: Cleanup + return.** The broker and egress tasks are shut down, service sidecars are torn down, and the per-sandbox control sockets are removed. `run_sandbox` returns `Ok(N)`. (With `--timeout`, an overdue sandbox is gracefully stopped, then force-killed, and the run returns 124; with `--ephemeral`, the worktree and branch are cleaned up regardless of exit code.)

**Stage 13: CLI exit.** `commands::run::execute` checks the exit code:
- `0` → prints "Sandbox 'fix-auth' exited cleanly." → `Ok(())` → process exits 0.
- `≠ 0` → prints "Sandbox 'fix-auth' exited with code N." → `std::process::exit(N)` so the OS sees the actual code (not anyhow's generic 1).

**Stage 14: Worktree preserved.** The git worktree at `~/.abox/state/worktrees/fix-auth` is still there. The user can `cd` into it, inspect what codex did, run `abox merge fix-auth` to integrate, or `abox stop fix-auth --clean` to throw it away.

That's the whole lifecycle. Every step has a single owner and a clear failure mode.

---

## Where to go next

- **[`docs/tutorial.md`](tutorial.md)** — actually do all of this on your machine in 10 minutes.
- **[`docs/decisions/`](decisions/)** — the architecture decision records for the choices that shaped abox.
- **[`docs/plans/`](plans/)** — historical and planned implementation work.
- **[`scripts/local/msb_e2e_test.sh`](../scripts/local/msb_e2e_test.sh)** — the canonical "is this thing working?" gate (`just e2e-runtime`, 49 assertions against real microVMs).
- **[`docs/future-work.md`](future-work.md)** — what's next after the priorities roadmap landed.
- **[`docs/decisions/003-https-credential-injection.md`](decisions/003-https-credential-injection.md)** — ADR for the TLS-terminating MITM proxy architecture.
