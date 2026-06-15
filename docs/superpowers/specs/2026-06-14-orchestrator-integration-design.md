# Orchestrator Integration Design (issues #24–#28)

## Context

abox today is optimized for two managed agents (`claude`, `codex`) driven from
the CLI. A third-party orchestrator (e.g. a custom `agent-runner`, a CI harness,
or `opencode` against a self-hosted model) that runs its own `--` command hits a
wall at every layer:

- **Input:** `--prompt`/`--prompt-file` are hard-restricted to the managed
  agents (`run.rs:437-461`, allowlist at `run.rs:200-206`). A custom command has
  no first-class way to receive a payload and must base64-stuff it into `--env`
  (ARG_MAX-bounded, leaks into the process environment).
- **Networking:** the guest can reach declared `[services]` Docker sidecars
  (postgres/redis/ollama/mysql) over the vsock bridge, but not a service the
  operator already runs on the host (e.g. an LLM gateway on `localhost:4000`).
- **Output:** no `--json` on `list`/`divergence`/`grant list`; integrators
  reverse-engineer the internal worktree path `~/.abox/worktrees/<task>`.
- **Noise:** virtiofsd logs `failed to change uid/gid back to root` at ERROR on
  every clean run, eroding trust and burying real errors.

These map to GitHub issues #24, #25, #27, #26, #28 respectively.

This spec covers all five as one coordinated change. Four of the five are new
*producers* of channels that already exist; none introduces a new transport.
The exception is the host-port bridge (#25), which is an explicit, gated hole in
the egress boundary and is designed accordingly.

## Security contract (the invariants this design must respect)

From `docs/explainer.md`, stated as designed properties:

1. The agent is **untrusted** (may run untrusted code or be prompt-injected).
2. There are exactly **two narrow egress channels**: the shim→policy bridge
   (CLI) and the HTTPS egress proxy (MITM + credential injection).
3. The guest has **no direct NIC-based outbound path**, even in `open` mode;
   network modes do not bypass the host mediation boundary.
4. "No network the guest can scan, **no SSRF vector**."
5. **Credentials never enter the VM.**
6. Every action is **provably attributed and audited**.

Each feature below states its relationship to these invariants.

## Goals

1. Give any `--` command a first-class, read-only data-in channel.
2. Make an existing host service reachable from the guest as a deliberate,
   auditable, mode-gated exception — never a silent one.
3. Make abox state and result collection a supported, machine-readable contract.
4. Keep clean runs quiet without hiding genuine errors.
5. Document the managed-agent constraint and the recommended self-hosted-model
   path honestly.

## Non-Goals

- A generic ad-hoc `--host-port` CLI flag (rejected on security grounds; see §2).
- Raw guest networking or a guest NIC.
- `abox cp` / arbitrary guest-path read-back (deferred; `abox path` covers the
  common case).
- Custom credential injection for host-port bridges.
- Env-var injection for host-port bridges (the operator chose the guest port).

---

## 1. `--input-file`: generic data-in (#24)

**Invariant alignment:** host→guest flow into the already read-only `aboxmeta`
share (`ro,nodev,nosuid`, `init.sh:104`). Opens no outbound path, exposes no host
capability. Same trust shape as `--prompt-file`, decoupled from the agent
allowlist. Aligned.

### 1.1 CLI

```
abox run --task t --input-file <hostpath>[:<guestname>] -- <any command>
```

- Repeatable (`Vec<InputFileArg>`).
- Independent of `--prompt`/managed-agent logic. No command rewriting.
- `guestname` defaults to `basename(hostpath)`.

### 1.2 Validation

- `guestname` must match `^[A-Za-z0-9._-]+$` (the regex already excludes `/`),
  and the exact components `.` and `..` are additionally rejected. This prevents
  traversal out of `inputs/` into reserved meta files or outside the meta dir.
- Host file must exist and be readable (mirror the `--prompt-file` check at
  `run.rs:420`).
- Enforce a per-file and total size sanity cap (default 64 MiB/file, 256 MiB
  total) so a runaway input cannot fill the host meta dir; configurable later if
  needed.

### 1.3 Threading

Add to `CreateSandboxParams` (`sandbox.rs:16`) and `VmConfig` (`vm.rs:38`):

```rust
pub input_files: Vec<InputFile>, // { host_path: PathBuf, guest_name: String }
```

### 1.4 Staging

Alongside `prompt.md` staging in `cloud_hypervisor.rs`:

- Create `meta_dir/inputs/`.
- Copy each host file to `meta_dir/inputs/<guest_name>` at mode `0444`.
- Staging under `inputs/` avoids collision with reserved meta names
  (`runner.sh`, `boot.json`, `prompt.md`, `services`, `credentials/`).

### 1.5 Guest contract

Injected via `env_vars` so `runner.sh` exports them like any other variable:

- `ABOX_INPUT_DIR=/abox-meta/inputs` whenever any input is staged.
- `ABOX_INPUT_FILE=/abox-meta/inputs/<guest_name>` additionally when exactly one
  input file is given (single-file convenience).

---

## 2. Host-port bridge: reach an existing host service (#25)

**Invariant alignment — explicit, gated exception.** A host-port bridge gives the
untrusted guest an unmediated TCP path to a host-loopback service. This collides
with invariants 2, 3, and 4: it is a third egress channel that bypasses both
designed ones, and it reintroduces a reachable host surface (SSRF/pivot). It can
also weaken invariant 5 when the bridged service holds upstream credentials
(e.g. an LLM gateway). It is therefore designed as a **deliberate,
version-controlled, mode-gated, per-connection-audited** hole — not as a sidecar.

### 2.1 Surface: config-only, no ad-hoc flag

Declared **only** in repo-committed config (reviewable in PR). There is no
`--host-port` CLI flag — a one-off invocation must not be able to silently open
an unmediated channel.

```toml
# .abox/project.toml
[[host_ports]]
guest = 4000   # agent connects to 127.0.0.1:4000 inside the guest
host  = 4000   # operator's existing service on host loopback
```

Add `host_ports: Vec<HostPortBridge>` to `ProjectConfig` (`project.rs:215`),
a dedicated array-of-tables rather than overloading the name-keyed `[services]`
map (which is typed by service name against `SERVICE_DEFS`).

### 2.2 Network-mode gating

- **Refused in `safe`** — a host-port bridge is by definition not "only the
  host-managed surface." A config declaring `[[host_ports]]` while the effective
  mode is `safe` is a hard error at resolve time, naming the offending entry.
- Permitted in `scoped` and `open`.

### 2.3 Mechanism (reuses existing plumbing, zero new guest code)

- The host-side splice is the existing `serve_service_bridge(socket, host_port)`
  (`services.rs:166`), which already connects to `127.0.0.1:host_port`. Host side
  stays loopback-only — the guest can never reach the host LAN, only services the
  operator runs on their own loopback.
- Allocate vsock ports continuing past the sidecar services:
  `vsock_port = SERVICE_VSOCK_BASE + service_bridges.len() + index`.
- Emit a `GuestServiceBridge`-shaped line (name `hostport-<host>`) into the
  existing `/abox-meta/services` file; the guest's existing socat loop
  (`init.sh:185-204`) tunnels it. No guest changes.

### 2.4 Auditing — per connection

- One `AuditEntry` at bridge **setup** (`request_type: "host-port-bridge"`,
  `target: "guest:<g>->host:<h>"`, `decision: allowed`).
- One `AuditEntry` per **connection** accepted in the `serve_service_bridge`
  accept loop (`request_type: "host-port-connect"`, same `target`). Payload is
  not visible (may be TLS); the connection event is. This makes any pivot attempt
  visible in the tamper-evident trail.

### 2.5 No env-var injection

Unlike sidecars (`ABOX_POSTGRES_URL` etc.), host-port bridges inject no env var;
the operator chose the guest port and knows it.

### 2.6 Documentation honesty

`docs/explainer.md`'s "no SSRF vector / no unmediated outbound" claims must be
amended to carve out this opt-in: in `scoped`/`open`, a repo may declare
`[[host_ports]]` that bridge specific host-loopback ports, and this is the one
operator-authorized exception, audited per connection.

---

## 3. Self-hosted model routing (positioning for #24 + #25)

The aligned way to reach a **network-reachable** self-hosted model (e.g.
vLLM-on-k8s) is the **egress proxy + a `scoped` egress rule** — mediated,
audited, credential injection available. This is the documented primary path.

The host-port bridge (§2) is the **escape hatch** for the narrow case of a
service bound to host **loopback** (e.g. LiteLLM on `localhost:4000`) that cannot
otherwise be reached. Even there, the recommended remedy is to bind the service
to a reachable interface and add a scoped egress rule; the bridge exists for when
that is not possible.

Docs (tutorial / explainer / future-work) and the response to integrators should
lead with the egress-proxy path and present `[[host_ports]]` as the gated
fallback.

---

## 4. Machine-readable output (#27)

**Invariant alignment:** host-side operator tooling; grants the guest nothing.
Aligned. One enforced check: `grant list --json` serializes rule *metadata*
(domain, header name, source name) and **never** resolved credential values —
the same boundary the existing table respects.

### 4.1 `--json` on `list`, `divergence`, `grant list`

Each gets an explicit serde struct so the schema is a committed contract:

- `list`: `[{ id, branch, state, pid, ahead, worktree_path }]`
- `divergence`: `[{ file, sandbox, status }]`
- `grant list`: `[{ domain, header, source, request_rules }]` — no secret values.

### 4.2 `abox path <task>`

New subcommand. Prints the host worktree path (`worktree_path` already on
`SandboxStatus`, `sandbox.rs:62`) to stdout and exits 0; unknown task → stderr +
nonzero exit. That path *is* the bind-mounted `/workspace`, so anything the agent
wrote there is collectable without depending on `~/.abox/worktrees/<task>`.

`abox cp <task>:<guestpath> <dest>` is deferred — `abox path` covers the common
case (agent writes under `/workspace`).

---

## 5. virtiofsd log noise (#26)

**Invariant alignment:** observability only; no boundary change. Aligned.

### 5.1 Root cause (confirm-benign-first)

virtiofsd's passthrough `seteuid`/`setegid`s to the caller per request then
restores to 0. Under abox's rootless `--sandbox=namespace` + `--uid-map`
(`cloud_hypervisor.rs:25-47`), the restore-to-0 has no `CAP_SETUID` for uid 0 in
that user namespace, so it `EINVAL`s — benign for abox's single-uid mapping. The
plan **verifies this is benign** before muting, so a real regression is not
hidden.

### 5.2 Fix

virtiofsd stderr is currently inherited (uncontrolled). Spawn each virtiofsd with
`Stdio::piped()` stderr and a reader task that forwards lines to `tracing`:

- Lines matching the exact known credential-restore message → `debug`.
- **All other lines pass through unchanged** at their original level. Match must
  be tight (exact message), never a loose `credentials` substring, so a genuine
  virtiofsd privilege error is never masked.

Applies to all virtiofsd spawns (workspace + auxiliary shares).

---

## 6. Documentation fix (#28)

Update the clap help for `--prompt`/`--prompt-file` to state they are
managed-agent-only (claude/codex) and to point at `--input-file` for generic
payloads. Mooted in spirit by #24, but the cross-reference is still worth it.

---

## 7. Testing

- **Unit:** `--input-file` spec parsing (with/without `:name`; rejects `..`,
  `/`, oversize); `[[host_ports]]` TOML round-trip; `safe`-mode rejection of
  `[[host_ports]]`; serialization snapshots of each `--json` schema;
  virtiofsd line-filter (benign line downgraded, other lines untouched);
  `grant list --json` contains no credential values.
- **Live (extend `scripts/local/e2e_test.sh` / `abox-live-validate`):** stage an
  input file and assert the guest sees `ABOX_INPUT_DIR`/`ABOX_INPUT_FILE`; bridge
  a host listener (`python -m http.server`) and reach it from the guest in
  `scoped` mode; confirm a `host-port-connect` audit entry is written; `abox path`
  returns the worktree dir; `--json` outputs parse.
- **#26:** manual confirmation the happy path is quiet.
- Clippy under `RUSTUP_TOOLCHAIN=stable` for CI parity.

---

## 8. Sequencing (one implementation plan)

1. **#28** help text — trivial, zero risk.
2. **#24** `--input-file` — establishes `input_files` threading + `inputs/`
   staging.
3. **#25** host-port bridge — config-only, `safe`-refused, per-connection audit;
   reuses the services metadata channel.
4. **#27** `--json` + `abox path`.
5. **#26** virtiofsd stderr capture/filter.

## 9. Decisions recorded

- Scope: one combined spec, all five issues.
- #24 guest contract: predictable dir under `/abox-meta/inputs/` **plus** env
  vars (`ABOX_INPUT_DIR`, and `ABOX_INPUT_FILE` for the single-file case).
- #25 surface: **config-only** (`[[host_ports]]`), **no `--host-port` flag**,
  **refused in `safe`**, **per-connection audit**. (Hardened from the initial
  "flag + config, all modes" proposal after measuring against the security
  contract.)
- #25 config shape: dedicated `[[host_ports]]` array, not folded into
  `[services]`.
- #25 vsock allocation: shares the sidecar range, continuing past it.
- #25 no env-var injection.
- #27: `abox path` now; `abox cp` deferred.
- Self-hosted models: egress-proxy path is primary/recommended; host-port bridge
  is the loopback-only escape hatch.
