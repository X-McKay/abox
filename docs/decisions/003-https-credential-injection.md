# ADR-003: HTTPS Credential Injection via TLS-Terminating Proxy

**Status:** Accepted
**Date:** 2026-04-09

## Context

abox sandboxes need to make authenticated HTTPS requests to external APIs
(Anthropic, OpenAI, GitHub, etc.). The original design used a simple TCP
CONNECT passthrough proxy that could not inspect or modify encrypted traffic.
Credentials had to be injected as environment variables inside the VM, which
meant secrets crossed the sandbox boundary and were accessible to the agent.

## Decision

Implement a TLS-terminating MITM (man-in-the-middle) proxy that:

1. **Generates a root CA** (`~/.abox/ca/root.crt` + `root.key`) and bakes it
   into the guest rootfs trust store at build time.
2. **Signs per-host leaf certificates** on the fly (cached by SNI hostname)
   using the root CA, so the guest trusts the proxy's TLS.
3. **Terminates TLS from the guest**, reads the plaintext HTTP request,
   injects credential headers (e.g., `x-api-key`, `Authorization: Bearer`)
   based on policy rules, then opens a new TLS connection to the real
   upstream using system root certificates (webpki-roots).
4. **Provides a bypass list** (`bypass_tls` in policy) for cert-pinned
   clients that would reject the MITM certificate. These domains use plain
   TCP passthrough.
5. **Allocates a per-sandbox listener port** and injects `HTTPS_PROXY` /
   `https_proxy` env vars into the guest boot metadata, so each sandbox
   gets its own proxy instance with audit attribution.

## Consequences

### Positive

- **Credentials never enter the VM.** The API key stays on the host; the proxy
  injects it into the wire just before forwarding to the upstream.
- **Policy-driven.** Each domain/header/env-var mapping is declared in
  `policies/default.toml`. Adding a new service is one TOML stanza.
- **Transparent to the agent.** The agent uses standard HTTPS libraries; the
  `HTTPS_PROXY` env var routes traffic through the proxy automatically.

### Negative / Risks

- **CA key is security-sensitive.** `~/.abox/ca/root.key` must be protected
  (0600 permissions). Compromise of this key allows impersonation of any
  domain to any guest that trusts the CA.
- **Cert-pinned clients break.** Applications that pin leaf or intermediate
  certificates (e.g., some SDKs, mobile clients) will reject the MITM cert.
  The `bypass_tls` list mitigates this but disables credential injection for
  those domains.
- **Rootfs rebuild on CA rotation.** Changing the CA requires rebuilding the
  guest rootfs image to update the embedded trust store. The `abox ca rotate`
  command automates this.
- **HTTP/1.1 only (initial).** The proxy parses HTTP/1.1 request headers for
  injection. HTTP/2 support is a future enhancement.

## Alternatives Considered

- **Environment-variable injection into the VM** (prior approach): simpler but
  exposes secrets inside the sandbox. Rejected for security reasons.
- **Sidecar proxy inside the VM** (e.g., mitmproxy): increases rootfs size,
  adds a process to manage, and still requires credential injection into the
  VM (to configure the sidecar). Rejected.
- **SDK-level credential providers** (e.g., IAM roles, OIDC): not universally
  supported across all target APIs and requires per-SDK integration. May be
  added later as a complement, not a replacement.

---

## Amendment: Credential File Support and Stub Injection (2026-04-12)

### Problem

OAuth-based tools such as Claude Code check for a local credential file (e.g.,
`~/.claude/.credentials.json`) before making any API calls. If that file is
absent or invalid, the tool refuses to start — it never reaches the network
layer where the MITM proxy could inject the real token. API key injection alone
is therefore insufficient for OAuth-gated tools.

### Solution

Two new mechanisms work together:

**1. Stub credential files** (`[guest] credential_files` in `~/.abox/config.toml`)

A credential file entry maps a host credential file to a guest path and
optionally specifies a `stub` — a JSON object with placeholder token values
written into the guest filesystem at sandbox boot time. The stub passes the
tool's local credential check (the file exists, has the right shape) without
containing any real token. Example:

```toml
[guest]
[[guest.credential_files]]
host = "~/.claude/.credentials.json"
guest = "/.claude/.credentials.json"

[guest.credential_files.stub.claudeAiOauth]
accessToken = "abox-proxy-managed"
expiresAt = 9999999999999
refreshToken = "abox-proxy-managed"
```

**2. Credential file source in egress policy**

Policy egress rules now accept either `env_var` (as before) or a
`credential_file` + `json_path` pair. `json_path` is a dot-separated path into
the JSON credential file on the host (e.g., `claudeAiOauth.accessToken`). The
proxy reads the real token from the host file at request time and injects it
into the outbound request — the stub value in the guest is never used on the
wire.

```toml
[[egress]]
domain = "api.claude.ai"
inject_header = "Authorization"
credential_file = "~/.claude/.credentials.json"
json_path = "claudeAiOauth.accessToken"
header_template = "Bearer {value}"
```

### Security property

The real credential never enters the VM. The stub token (`"abox-proxy-managed"`)
is worthless — it satisfies the tool's local file check but is intercepted and
replaced by the proxy before any request reaches the upstream API. An agent that
exfiltrates the credential file gets only the placeholder.

### Per-sandbox egress proxy via vsock

The per-sandbox `EgressProxyServer` is now spawned from `run_sandbox()`, closing
the gap noted in the original ADR. Because the guest has no direct network access
(no virtio-net), the proxy is reached via vsock:

- Host: proxy listens on vsock port 5001 (one instance per sandbox).
- Guest `init.sh`: brings up the loopback interface and runs `socat` to bridge
  vsock CID 2 port 5001 → TCP `127.0.0.1:18443`.
- Guest environment: `HTTPS_PROXY=http://127.0.0.1:18443` (injected by the
  orchestrator; replaces the former `10.0.2.2:<port>` QEMU user-mode address).

### Node.js CA trust

Node.js does not use the system trust store by default. The orchestrator injects
`NODE_EXTRA_CA_CERTS=<path-to-abox-root.crt>` into the guest boot metadata so
that Node.js-based tools (Claude Code, Codex CLI) trust the MITM certificate
without any rootfs change.

### See also

- [`docs/superpowers/specs/2026-04-12-credential-forwarding-design.md`](../superpowers/specs/2026-04-12-credential-forwarding-design.md) — full design spec
- [`docs/superpowers/plans/2026-04-12-credential-forwarding.md`](../superpowers/plans/2026-04-12-credential-forwarding.md) — implementation plan with TDD tasks
