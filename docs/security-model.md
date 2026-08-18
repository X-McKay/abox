# Security model

This page states abox's threat model and the invariants the implementation is
held to. It complements [`runtime.md`](runtime.md) (how the runtime works),
[`audit-log.md`](audit-log.md) (audit tamper-evidence), and
[ADR-008](decisions/008-microsandbox-runtime-and-product-boundary.md) (the
runtime/product boundary).

## Threat model

### Untrusted

Everything that runs, or can influence what runs, inside the sandbox:

- **The repository under work.** Its contents, build scripts, and
  `.abox/project.toml` are attacker-influenced until explicitly trusted, and
  even a trusted repo's *code* stays untrusted. Repo config can express task
  behavior and environment intent; it can never select the runtime, choose a
  guest image, or weaken the isolation boundary.
- **The agent.** Prompt injection, a malicious model response, or a
  compromised agent CLI are all in scope. The sandbox boundary must hold
  without the agent's cooperation.
- **All guest processes and the guest kernel.** A fully compromised guest —
  root in the guest, arbitrary guest kernel code — must still be confined by
  the hardware virtualization boundary and unable to widen its authorization.
- **Everything the agent generates:** code, commits, command invocations,
  HTTP requests, and any bytes it writes into the workspace.

### Trusted

- **The abox host process** (orchestrator, CLI) and the host-side brokers
  (`CommandBroker`, the HTTPS request broker / egress proxy, `abox-proxyd`).
- **Host-owned policy and config files:** `~/.abox/config.toml`,
  `~/.abox/policies/*.toml`, and the image manifest embedded in the abox
  binary.
- **The MicroSandbox host runtime** (the `msb` process and libkrun). It is
  part of the trusted computing base: abox delegates VMM, image handling, and
  mount mechanics to it, pins it exactly, and qualifies upgrades through
  dedicated PRs (see [`runtime-upgrades.md`](runtime-upgrades.md)).
- **The host OS and hypervisor** (KVM / Hypervisor.framework). An attacker
  with root on the host is out of scope — they already own the credentials
  abox protects.

## Security invariants

These are the properties test suites and reviews defend. A change that
weakens one of them is a security regression regardless of what feature it
enables.

1. **Host isolation.** Agent code executes only inside a hardware-isolated
   microVM. No abox feature gives the guest a host shell, host filesystem
   access outside declared mounts, or a way to spawn host processes except
   through the policied command broker.

2. **Workspace isolation.** Each sandbox sees exactly one worktree at
   `/workspace` (plus declared caches/inputs). It cannot reach other tasks'
   worktrees, the primary checkout, or the rest of the host filesystem.
   Writes land only in its own worktree; merge back to the main branch is an
   explicit, host-side operation (`abox merge`).

3. **Command authorization.** Privileged host commands (`git`, `gh`, `aws`)
   exist in the guest only as shims. Every invocation crosses the broker and
   is evaluated against host policy — e.g. `git push origin <branch>` may be
   allowed while `git push --force` and `gh repo delete` are denied. A lost
   broker response is never retried in a way that re-executes a privileged
   command.

4. **Credential isolation.** Real secrets never enter the guest. The guest
   receives placeholder stubs; the host injects real values at request time
   (MITM header injection for HTTP, host-side execution for CLIs). Rules
   delegated to native runtime substitution pass a *source reference* (host
   env var name), never a value, so secrets cannot persist in runtime state.

5. **Request authorization.** Credential-bearing HTTPS requests are
   authorized per-domain, and — for broker-enforced rules — per method/path
   (`request_rules`). A rule that opts into native substitution but cannot be
   represented exactly fails at launch; enforcement is never silently
   downgraded (see
   [`credential-scoping.md`](credential-scoping.md#who-enforces-a-rule-credentialexecutionstrategy)).

6. **Attribution.** Every brokered action is attributed to its sandbox from
   the per-sandbox host route (the Unix socket the runtime binds for that
   sandbox), never from guest-asserted identity. A guest cannot impersonate
   another sandbox.

7. **Audit integrity.** Every brokered command and egress request is recorded
   in the keyed hash-chained audit log. A compromised guest cannot forge,
   rewrite, or truncate it undetected ([`audit-log.md`](audit-log.md)).

8. **Fail closed.** Configurations the runtime cannot represent exactly are
   rejected at policy-load or launch time — never approximated. Examples:
   `native_substitution` in `safe` mode, a profile with no image mapping, an
   unknown network scope entry.

## Network plan invariants

abox compiles user-facing network modes into runtime plans in abox policy
(`compile_runtime_network_plan`); the runtime translates them mechanically
and never widens them. The invariants, asserted by unit release gates on the
compiled policy and validated live against real microVMs:

- **`safe`** — the guest has no network path at all. All egress rides the
  audited abox proxy channels (command broker + HTTPS egress proxy).
- **`scoped`** — direct egress only to resolved bundle hosts and explicitly
  approved domains, DNS-pinned and SNI-verified by the runtime; TCP 443 plus
  DNS to the gateway; everything else refused.
- **`open` ≠ unrestricted.** `open` allows broad *public-internet* egress
  only. Loopback, RFC1918/ULA/CGN ranges, link-local, cloud metadata
  endpoints, multicast, and the host itself are denied by explicit
  first-match rules in every native plan. Egress is TCP 443 + DNS; ingress is
  denied entirely.
- **The mediated channel stays primary.** `HTTPS_PROXY` points at the abox
  egress proxy in every mode, so proxy-aware clients keep per-domain audit
  and credential injection even when a native plan exists.
- The one operator-authorized exception is a declared `[[host_ports]]`
  bridge (refused in `safe` mode, audited per connection) — see
  [`explainer.md`](explainer.md) section 5.

## Defense in depth

- The hardware virtualization boundary backs every guest-side invariant, so
  no single guest-side bug (shim, bridge, agent CLI) is load-bearing for host
  isolation.
- The broker and egress proxy enforce policy *on the host*, so guest
  compromise cannot bypass them — only attempt requests that policy then
  denies (and audits).
- The audit chain is keyed with a host-only HMAC key, so even full guest
  compromise leaves tamper-evident history.
- **Future work:** OS-level confinement of the MicroSandbox runtime process
  itself (Landlock on Linux, with AppArmor as optional operator-managed
  hardening) is under a feasibility gate in
  [ADR-009](decisions/009-runtime-process-confinement.md). Today the `msb`
  process runs with the invoking user's ambient authority; its compromise is
  equivalent to host-user compromise, which is why it is pinned exactly and
  upgraded only through qualified PRs.

## Review and trust handoffs

Sandbox isolation prevents the guest from directly reaching host authority; it
does not make agent-authored repository content safe for a host user to trust
later. Build scripts, editor settings, CI configuration, and dependency changes
can cause effects when reviewed, merged, or executed outside the sandbox.

`abox merge` is therefore an explicit host-side action. When host merge
validation is configured, it blocks selected high-risk path, executable, and
size changes before it mutates the base branch, and requires per-path operator
acknowledgement for review-required paths. This is a review gate, not a claim
that arbitrary merged source code is safe to execute; users must still review
the diff and apply their normal repository controls.
