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
