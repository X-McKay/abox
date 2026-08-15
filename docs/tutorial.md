# abox in 10 minutes

A zero-to-first-sandbox walkthrough for a Rust-familiar developer who has never seen abox before. Real commands, real captured output. By the end you will have booted a MicroSandbox microVM, run a guest command inside it, watched the audit log attribute the call to your sandbox, and cleaned up.

If you want the *why* — what each component does and how they fit together — read [`docs/explainer.md`](explainer.md) after this. This page is "do".

---

## 1. Prerequisites

- **Linux (x86_64 or aarch64)** with KVM available to your user, **or macOS
  on Apple Silicon** (Hypervisor.framework — no setup needed). On Linux,
  check:

  ```bash
  ls -la /dev/kvm
  ```

  Expected output (the `+` ACL or your username in the `kvm` group):

  ```
  crw-rw----+ 1 root kvm 10, 232 Apr  8 04:00 /dev/kvm
  ```

  If you don't see `+` or aren't in the `kvm` group: `sudo usermod -aG kvm $USER`, then log out and back in.

- **Rust toolchain** via `rustup`.

- **`just` command runner**: `cargo install just`.

- **About 1 GB of disk** for the MicroSandbox runtime assets, the guest OCI
  image cache, and your `target/` build directory.

---

## 2. Clone and build

```bash
git clone https://github.com/X-McKay/abox.git
cd abox
cargo build --release
```

Build takes a couple of minutes the first time and hits the disk for ~700 MB of build artifacts under `target/`.

---

## 3. First-run setup

First, put the newly built binary on your `PATH` so you can run it:

```bash
export PATH="$PWD/target/release:$PATH"
```

Now run the guided setup wizard:

```bash
abox init
```

This walks every prerequisite in order:

1. Checks hardware virtualization (`/dev/kvm` on Linux, Hypervisor.framework on macOS).
2. Installs the MicroSandbox runtime assets (`msb` + libkrunfw guest firmware) into `$MSB_HOME` (default `~/.microsandbox`).
3. Generates the abox root CA (for HTTPS credential injection).
4. Writes `~/.abox/config.toml`.
5. Installs `policies/default.toml` to `~/.abox/policies/default.toml`.
6. Detects managed-agent credentials (Claude Code / Codex) on the host.
7. Checks host-staged guest binaries and verifies each requested guest profile resolves to an OCI image, printing the image it will pull.

There is no VM bootstrap, kernel download, or rootfs build — guest
environments are OCI images pulled on first use. (The old
`bootstrap_vm.sh` flow still exists for the deprecated Cloud Hypervisor
backend only; see [`vm-setup.md`](vm-setup.md).)

The default policy allows common `git` and constrained `gh` operations, denies dangerous mutations such as `git push --force`, and default-denies unknown HTTPS egress. Matching HTTPS requests go through the MITM proxy, which injects host-side credentials for Anthropic and OpenAI/Codex. GitHub stays on the host through managed `git` / `gh` execution. Domains listed in `bypass_tls` remain plain TCP passthrough for cert-pinned clients.

To verify your environment is ready, run:

```bash
abox doctor
```

If you already know you want the official ecosystem profiles too, name them
up front and `abox init` will verify each resolves in the image manifest and
print the image it will pull:

```bash
abox init --profile node --profile python --profile rust
```

---

## 5. Your first sandbox

You need a git repo for the agent to work in. We'll make a tiny scratch one:

```bash
mkdir -p /tmp/abox-tutorial && cd /tmp/abox-tutorial
git init -q demo-repo
cd demo-repo
git config user.email tut@abox.test
git config user.name tutorial
echo "# demo" > README.md
git add README.md && git commit -q -m "init"
```

Now boot a sandbox in this repo:

```bash
abox run --task hello -- /bin/sh -c "echo hello from inside the sandbox; /usr/local/bin/git log --oneline"
```

You'll see the sandbox start, your command's output, and a clean exit
(this capture is from the legacy Cloud Hypervisor backend — the exact log
lines differ under MicroSandbox, and the first run per profile also pulls
the guest OCI image):

```
Sandbox 'hello' starting...
[INFO  abox_core::adapters::cloud_hypervisor] MicroVM started sandbox_id=hello pid=2271574 memory_mib=512 vcpus=1
[INFO  abox_core::proxy_bridge] proxy bridge listening socket=…/r/vsock-hello.sock_5000 attribution=Fixed("hello")

==> abox guest init: online
    kernel: 6.16.9+
    root:   /dev/root ext4

==> running /abox-meta/runner.sh
hello from inside the sandbox
ea68f62 init

==> abox guest init: poweroff (rc=0)

Sandbox 'hello' exited cleanly.
```

What just happened, in one paragraph: `abox run` created a git worktree on a new branch `agent/hello`, started a MicroSandbox microVM from the `base` guest image with that worktree bind-mounted read-write at `/workspace`, bound the per-sandbox broker and egress sockets on the host, and exec'd your command inside the guest as the unprivileged `abox` user. The `git` invocation inside the guest is actually a symlink to `abox-shim`, which forwarded the request over vsock to the host's per-sandbox proxy bridge. The bridge evaluated the policy (allow), executed the real `git log` on the host worktree, returned the output, and the shim printed it. When the command exited, its exit code propagated directly back through the runtime and the sandbox stopped.

---

## 6. Inspect state

After the sandbox exits, the worktree is preserved by default so you can `cd` into it and look around. Show the list:

```bash
abox list
```

```
ID               BRANCH                   STATE      PID      AHEAD
----------------------------------------------------------------------
hello            agent/hello              stopped    0        0

1 sandbox(es) active
```

Show the divergence (what each agent has changed compared to `main`):

```bash
abox divergence
```

For our `hello` sandbox there are no changes — the agent only ran `echo` and `git log`, no commits. If you had run an agent that wrote files, you'd see them here with `Added` / `Modified` / `Deleted` tags.

---

## 7. Merge and clean up

If the agent had made commits you wanted to keep, you'd merge:

```bash
abox merge hello
```

For this tutorial there's nothing to merge. Just clean up:

```bash
abox stop hello --clean
```

```
Sandbox 'hello' stopped and cleaned up.
```

`--clean` removes the worktree and deletes the `agent/hello` branch. Without it, the worktree is preserved and you can resume / inspect later.

Confirm:

```bash
abox list
# No active sandboxes.
```

---

## 8. Repo-owned workflow

The first sandbox above used only host config. For a repo you plan to work in
repeatedly, add repo-owned behavior explicitly:

```bash
mkdir -p .abox prompts

cat > .abox/project.toml <<'EOF'
[network]
mode = "scoped"
bundles = ["npm-public"]

[environment]
profile = "node"
caches = ["npm"]
prepare = ".abox/prepare.sh"
watch = ["package-lock.json"]

[agent]
default_prompt_file = "prompts/fix-auth.md"
EOF

cat > .abox/prepare.sh <<'EOF'
#!/bin/sh
set -e
npm ci --ignore-scripts --no-fund --no-audit
EOF
chmod +x .abox/prepare.sh

echo "Review the auth flow and suggest a fix." > prompts/fix-auth.md

abox project validate
abox project trust
abox env warm
abox run --task fix-auth -- codex
```

That flow gives you:

1. A repo-owned network mode (`safe`, `scoped`, or `open`)
2. An official guest profile (`base`, `node`, `python`, or `rust`)
3. Durable per-project caches
4. A repeatable prepare step that runs inside the real guest
5. First-class prompt-file support for bare `claude` and `codex`

Two profile-specific notes from the real validation matrix:

- Python prepare flows should prefer a virtualenv-based `uv` workflow rather
  than `uv pip install --system`, because the guest Python is intentionally
  externally managed.
- The current `rust` guest profile ships `rustc/cargo 1.76.0`. Repos that
  require Cargo edition 2024 or Cargo.lock v4 need a newer guest toolchain
  before `abox env warm` will succeed.

Use `abox project explain` at any time to review the effective repo behavior
that was approved, including the selected profile and widened network scope.

### Reaching a self-hosted model

Prefer the mediated path: if your model endpoint (e.g. vLLM on Kubernetes) is
reachable over the network, add it as a `scoped` egress rule so requests stay
behind the abox proxy (policy-checked, audited, credential injection available).

Only when the model gateway is bound to host loopback (e.g. a LiteLLM gateway on
`localhost:4000`) and cannot be exposed otherwise, declare a host-port bridge:

```toml
# .abox/project.toml
[network]
mode = "scoped"   # host-port bridges are refused in "safe" mode

[[host_ports]]
guest = 4000
host  = 4000
```

A declared host-port bridge counts as a `scoped` addition, so `mode = "scoped"`
with only `[[host_ports]]` is valid — no egress domain is required, and egress
stays restrictive. The agent then reaches the gateway at `127.0.0.1:4000` inside
the guest. Pair this with `--input-file` to hand a custom runner its task payload.

## 9. What just happened?

In about 10 minutes you:

1. Installed the MicroSandbox runtime assets and abox config with one command.
2. Booted a real microVM, mounted a git worktree into it, and executed a guest command inside it.
3. Watched the policy proxy intercept a `git` call from the guest, evaluate it against your policy, and execute it on the host with attribution.
4. Saw the guest agent's exit code propagate cleanly back to your shell.
5. Cleaned everything up.

If you want to know **why** every piece exists (why we use vsock instead of TCP, what the shim is doing under the hood, how the runtime stages the guest), read [`docs/explainer.md`](explainer.md) and [`docs/runtime.md`](runtime.md). They're longer but cover every component in depth.

If you want to set up your own policies, see [`policies/default.toml`](../policies/default.toml) as a starting point and [`docs/decisions/001-architecture.md`](decisions/001-architecture.md) for the rationale.

If something didn't work, [`docs/runtime.md`](runtime.md#troubleshooting) has the top failure modes and their fixes ([`docs/vm-setup.md`](vm-setup.md#troubleshooting) covers the legacy backend).
