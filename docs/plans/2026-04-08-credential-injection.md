# HTTPS Egress Credential Injection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** DEFERRED — this is a multi-day TLS-termination project that was explicitly carved out of the [`vm-e2e-hardening`](2026-04-08-vm-e2e-hardening.md) branch. The spec lives here so a future session can pick it up cleanly.

**Goal:** Replace `abox-proxyd::egress_proxy` (currently a passthrough TCP CONNECT tunnel) with a TLS-terminating MITM proxy that injects API-key headers (e.g. `x-api-key: $ANTHROPIC_API_KEY`, `Authorization: Bearer $OPENAI_API_KEY`) on outbound HTTPS requests, based on destination host. Resolves backlog items **F3** and **S2** from `docs/backlog/2026-04-08-vm-e2e-mvp-followups.md`.

**Architecture:**
- Generate a self-signed root CA at first run (`~/.abox/ca/root.{crt,key}`).
- Bake the CA cert into the guest rootfs at `/etc/ssl/certs/abox-ca.crt`. Re-run `update-ca-certificates` (or equivalent) at boot so guest TLS clients trust it.
- Bind a per-sandbox egress listener (e.g. `127.0.0.1:<unique-port>`); the orchestrator passes that port to the guest via `runner.sh` as `HTTPS_PROXY=http://10.0.2.2:<port>`.
- The egress proxy accepts HTTP CONNECT, generates a leaf cert for the requested SNI on the fly (signed by the abox CA), terminates the inner TLS, reads the request, looks up the matching `EgressRule`, injects the configured header from the host environment variable, then opens a new outbound TLS connection to the real upstream and proxies bidirectionally.
- Audit log entries for egress are now keyed off the per-sandbox listener port (or a per-sandbox bridge instance), resolving S2.

**Tech Stack:** Existing `abox-proxyd` (hyper, tokio), new deps: `rcgen` (CA + leaf cert generation), `rustls` (TLS termination), `tokio-rustls`, `webpki` (cert validation against the abox CA inside the guest).

**Out of scope:**
- HTTP/3 (QUIC). HTTPS/1.1 + HTTP/2 only.
- Egress to non-HTTPS endpoints (plain HTTP). Use the existing CLI proxy path.
- Re-implementing the CLI proxy (already correct via `proxy_bridge`).
- Certificate pinning workarounds for clients that pin the upstream cert chain (e.g., Go binaries with embedded roots, mobile clients). These will break and need to be either added to a per-policy bypass list or excluded from the proxy entirely.

---

## Risks and known sharp edges

1. **Certificate pinning.** Some clients pin the upstream's leaf or intermediate cert. Anthropic's official SDKs use stock OS roots (no pinning), so they should work; OpenAI's SDKs likewise. Go binaries with `crypto/x509`'s embedded root pool (used by some `cli` tools) will reject the abox CA. **Mitigation:** maintain a `bypass_pinned: Vec<String>` list in the policy file; matching destinations skip MITM and use straight CONNECT.
2. **Performance.** Every HTTPS connection now incurs two TLS handshakes (guest↔proxy + proxy↔upstream) and two encrypt/decrypt loops. Should be fine for the agent use case (low connection count, low byte volume), but a high-throughput agent will notice.
3. **Storing the CA private key on disk.** New sensitive asset. Mode 0600, owned by the user, in `~/.abox/ca/`. Document this prominently.
4. **CA rotation.** A leaked CA = ability to MITM the user's machine. Provide an `abox ca rotate` command that regenerates and re-bakes the rootfs.
5. **SNI vs Host header mismatch.** Some clients send a different `Host:` header than the SNI. The proxy must match policy on the SNI (which is what the cert is for) and verify the inner request's Host matches.

---

## Task 1: CA generation + storage

**Files:**
- Create: `crates/abox-core/src/ca.rs` (generate, load, persist a root CA)
- Modify: `crates/abox-core/src/lib.rs` (register module)
- Modify: `crates/abox-core/Cargo.toml` (add `rcgen = "0.13"`, `time = "0.3"`)
- Test: inline `#[cfg(test)]` in `ca.rs`

- [ ] **Step 1: Write the failing test for `RootCa::generate_and_persist(dir)`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let ca = RootCa::generate_and_persist(tmp.path()).unwrap();

        // Persisted files exist with correct modes (0600 for the key).
        assert!(tmp.path().join("root.crt").exists());
        assert!(tmp.path().join("root.key").exists());
        let mode = std::fs::metadata(tmp.path().join("root.key"))
            .unwrap()
            .permissions()
            .mode() & 0o777;
        assert_eq!(mode, 0o600);

        // Re-load from disk: same cert + key.
        let loaded = RootCa::load(tmp.path()).unwrap();
        assert_eq!(ca.cert_pem, loaded.cert_pem);
    }

    #[test]
    fn test_generate_and_persist_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let ca1 = RootCa::generate_and_persist(tmp.path()).unwrap();
        let ca2 = RootCa::generate_and_persist(tmp.path()).unwrap();
        assert_eq!(ca1.cert_pem, ca2.cert_pem); // didn't regenerate
    }
}
```

- [ ] **Step 2: Run the test; it fails** because `RootCa` doesn't exist.

- [ ] **Step 3: Implement `RootCa`** using `rcgen::CertificateParams` with a 10-year validity, CN = "abox sandbox CA", BasicConstraints = CA, KeyUsage = certSign + crlSign.

- [ ] **Step 4: Run the test; expect pass.**

- [ ] **Step 5: Commit** `feat(core): introduce RootCa for the sandbox MITM proxy`.

---

## Task 2: Leaf cert generator

**Files:**
- Modify: `crates/abox-core/src/ca.rs` — add `RootCa::sign_leaf(sni: &str)`

- [ ] **Step 1: Write a failing test for `sign_leaf(sni)`** that verifies the returned cert chain validates against the root, has the requested SAN, and has a 30-day validity.

- [ ] **Step 2: Implement** `sign_leaf` using `rcgen::CertificateParams` with `subject_alt_names = [SanType::DnsName(sni)]` and signed by the root CA.

- [ ] **Step 3: Add an in-memory cache** keyed by SNI so the same connection doesn't regenerate certs. `RwLock<HashMap<String, Arc<CertifiedKey>>>`.

- [ ] **Step 4: Test + commit.**

---

## Task 3: Bake the CA cert into the guest rootfs

**Files:**
- Modify: `scripts/build_rootfs.sh` (copy the CA cert into the staged rootfs)
- Modify: `scripts/bootstrap_vm.sh` (call `RootCa::generate_and_persist` first via a tiny rust binary, OR shell out to rcgen via `cargo run --release -p abox-core --bin abox-ca-init`)
- Modify: `guest/init.sh` (run `update-ca-certificates` or similar, OR document that Alpine miniroot needs `ca-certificates` apk)
- Possibly modify: rootfs to install `ca-certificates`

**Open question:** Alpine miniroot doesn't include `ca-certificates` by default. Two options:
  a) `apk add --no-cache ca-certificates` during rootfs assembly (needs apk + a working alpine repo at build time)
  b) Hand-place the CA as `/etc/ssl/certs/abox-ca.pem` and rely on the fact that openssl reads anything matching `*.pem` in that dir

Option (b) is simpler and avoids a network dep at rootfs build time. Document the assumption.

- [ ] **Step 1: Add `RootCa::generate_and_persist` invocation to `bootstrap_vm.sh`** (via `cargo run` or a tiny dedicated binary).

- [ ] **Step 2: Modify `build_rootfs.sh`** to copy `~/.abox/ca/root.crt` into `$STAGE/etc/ssl/certs/abox-ca.pem`.

- [ ] **Step 3: Add a smoke test** to phase 6 of `e2e_test.sh`: `abox run -- /bin/sh -c 'openssl s_client -connect anthropic.com:443 -CAfile /etc/ssl/certs/abox-ca.pem 2>&1 | head'`. Verify it doesn't error on CA validation (won't actually MITM yet — that's the next task).

- [ ] **Step 4: Test, document, commit.**

---

## Task 4: TLS-terminating proxy core

**Files:**
- Modify: `crates/abox-proxyd/src/egress_proxy.rs` (or rewrite as `crates/abox-core/src/egress.rs`)
- Modify: `crates/abox-proxyd/Cargo.toml` (add `rustls`, `tokio-rustls`, `webpki-roots`)

- [ ] **Step 1: Write a failing integration test** with a local echo HTTPS server using a self-generated cert; the test connects via the new proxy and asserts the bytes round-trip.

- [ ] **Step 2: Implement `accept_connect` handler** that:
  1. Reads the HTTP CONNECT request line, extracts the SNI/host
  2. Sends `200 OK` to the client
  3. Calls `RootCa::sign_leaf(host)` to mint a leaf cert
  4. Wraps the client socket in `tokio_rustls::server::TlsAcceptor` with a `ServerConfig` that uses the minted cert
  5. Reads the inner HTTP/1.1 or HTTP/2 request
  6. Connects outbound to `host:443` with `webpki-roots` trust anchors
  7. Forwards the request and response, with header injection in step 7 of the next task

- [ ] **Step 3: Test + commit.**

---

## Task 5: Header injection based on policy

**Files:**
- Modify: `crates/abox-core/src/policy.rs` — add `EgressInjection` returned alongside the `EgressRule`
- Modify: the egress proxy to call `policy.evaluate_egress(host)` and inject the named header from the configured env var

- [ ] **Step 1: Write a failing test** that:
  1. Sets `ANTHROPIC_API_KEY=test-key-12345` in the host env
  2. Configures policy with `domain = "api.anthropic.com"`, `inject_header = "x-api-key"`, `env_var = "ANTHROPIC_API_KEY"`
  3. Sends a request via the proxy to a local mock at `api.anthropic.com:443` (use `/etc/hosts` override or a custom DNS resolver)
  4. Asserts the upstream sees `x-api-key: test-key-12345`

- [ ] **Step 2: Implement** `inject_header` step in the proxy after the inner request is parsed and before forwarding upstream.

- [ ] **Step 3: Test + commit.**

---

## Task 6: Per-sandbox listener for audit attribution (S2)

**Files:**
- Modify: `crates/abox-core/src/sandbox.rs::run_sandbox` (start a per-sandbox egress listener bound to a free port)
- Modify: the orchestrator's runner_script generation to set `HTTPS_PROXY=http://10.0.2.2:<port>` in the guest env

- [ ] **Step 1: Allocate a free port per VM.** Bind to `127.0.0.1:0`, capture the bound port, pass it into `BootMeta::env`.

- [ ] **Step 2: Each per-sandbox listener uses `SandboxAttribution::Fixed(task_id)`** for its audit entries — provably attributed because the socket itself binds to a single VM's `HTTPS_PROXY` target.

- [ ] **Step 3: Test + commit.**

---

## Task 7: Bypass list for cert-pinned clients

**Files:**
- Modify: `crates/abox-core/src/policy.rs` — add `EgressBypass { domains: Vec<String> }` field to `PolicyFile`
- Modify: the proxy to short-circuit MITM for matching domains (use straight CONNECT pass-through with the existing audit attribution)

- [ ] **Step 1: Test + commit.**

---

## Task 8: End-to-end test against real upstream

**Files:**
- Modify: `scripts/e2e_test.sh` phase 6

- [ ] **Step 1: Add an assertion** that `abox run -- curl -s https://api.anthropic.com/v1/health` returns 200 (or whatever the relevant Anthropic health endpoint is) and the upstream sees the injected header. Skipping if `ANTHROPIC_API_KEY` is not set in the test environment.

- [ ] **Step 2: Test + commit.**

---

## Task 9: ADR-003 + docs

**Files:**
- Create: `docs/decisions/003-https-credential-injection.md`
- Modify: `README.md`, `docs/explainer.md`, `docs/vm-setup.md`

- [ ] **Step 1: Write ADR-003** documenting the TLS-termination decision, the alternatives considered (per-rule transparent proxies, sidecar containers, client-side credential helpers), and the cert-pinning bypass list.

- [ ] **Step 2: Update README + explainer** to remove the `passthrough` note from the egress section and reference the new behavior.

- [ ] **Step 3: Document the security implications** in `docs/vm-setup.md`:
  - The CA private key under `~/.abox/ca/` is highly sensitive.
  - The CA only signs certs for hosts with policy entries.
  - The user can `abox ca rotate` to regenerate.

- [ ] **Step 4: Commit.**

---

## Task 10: CA rotate command

**Files:**
- Create: `crates/abox-cli/src/commands/ca.rs`
- Modify: `crates/abox-cli/src/main.rs` (register the `Ca` subcommand)

- [ ] **Step 1: Implement `abox ca rotate`** — regenerate the CA, rebuild the rootfs, warn that any existing sandboxes will need to be restarted to pick up the new trust root.

- [ ] **Step 2: Test + commit.**

---

## Effort estimate

Roughly 10 tasks × 2-4 hours each = **2-3 working days** for an experienced Rust + TLS engineer. The biggest unknowns are:

1. Whether stock Anthropic / OpenAI / GitHub APIs work cleanly with the abox CA, or whether their official SDKs do unexpected pinning.
2. How `update-ca-certificates` interacts with the Alpine miniroot — Alpine uses BusyBox `update-ca-certificates` from `ca-certificates` apk; without that apk we have to drop the cert directly into `/etc/ssl/certs/`.
3. The interplay between per-sandbox listener ports and the kernel-level NAT for the guest's outbound traffic (currently the guest has no networking at all; we'd need to add a vsock-based egress channel OR a usermode TCP proxy via slirp). This is the largest unknown and may require an additional task.

## Acceptance criteria

- Phase 6 of `e2e_test.sh` includes an assertion that an `ANTHROPIC_API_KEY` set on the host is injected into a guest curl call to `api.anthropic.com`.
- The audit log shows the request with `sandbox_id=<task>` (closing S2).
- `cargo test --workspace` includes at least one positive test (header injected) and one negative test (no policy match → request denied or passed unmodified).
- ADR-003 lands in `docs/decisions/`.
- `cargo clippy -- -D warnings` clean.
- README's "egress" claim is honest.

## Why this isn't in `vm-e2e-hardening`

The hardening branch (Apr 8 2026) was time-boxed to drain the P0/P1 backlog from `vm-e2e-mvp`. F3 is the one L-sized item in that backlog and would dominate the entire session, leaving no room for the other 12 fixes. Splitting it into its own spec lets a future session tackle it with full focus.
