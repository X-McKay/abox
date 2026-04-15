# Credential Forwarding via Stub Files + Proxy Injection

**Date:** 2026-04-12
**Status:** Approved
**Relates to:** ADR-003 (HTTPS Credential Injection via TLS-Terminating Proxy)

## Problem

Agent tools like Claude Code authenticate via OAuth tokens stored in local
credential files (e.g., `~/.claude/.credentials.json`). Inside abox guest VMs
these files don't exist, so the tool shows "not logged in" and refuses to work.

Copying real credentials into the VM would violate ADR-003's core principle:
credentials never enter the sandbox.

## Solution

Split the credential into two halves:

1. **Stub file in the guest** — contains enough structure to pass the tool's
   local auth check (non-empty token, valid scopes, far-future expiry) but no
   real secret. The stub's token value is a known placeholder like
   `"abox-proxy-managed"`.

2. **Real credential injection at the proxy** — the host-side MITM egress proxy
   reads the real token from the host's credential file and replaces the stub's
   placeholder header with the real one before forwarding to the upstream API.

The agent never sees the real credential. The stub is worthless without the
proxy.

## Architecture

```
Guest                          Host MITM Proxy                    Upstream
─────                          ──────────────                    ────────
Tool reads stub file
  ✓ accessToken exists
  ✓ scopes pass check
  ✓ expiresAt in future

Sends request ─────────────►  Terminates TLS
  Authorization:               Reads real token from host
  Bearer abox-proxy-managed    credential file
                               Strips placeholder header
                               Injects real header ──────────►  Authorization:
                                                                Bearer <real-token>
                               ◄────────── Response ◄──────────
◄───────── Response
```

## Config Schema

### Guest credential files (`~/.abox/config.toml`)

New `[guest]` section on `AboxConfig`:

```toml
[guest]

[[guest.credential_files]]
host = "~/.claude/.credentials.json"
guest = "/.claude/.credentials.json"
mode = "0600"

[guest.credential_files.stub]
[guest.credential_files.stub.claudeAiOauth]
accessToken = "abox-proxy-managed"
refreshToken = "abox-proxy-managed"
expiresAt = 9999999999999
scopes = ["user:inference"]
subscriptionType = "pro"
```

Each entry:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `host` | string | yes | Source path on host. Supports `~` expansion. |
| `guest` | string | yes | Absolute destination path inside the VM. |
| `mode` | string | no | Unix permissions. Default: `"0600"`. |
| `stub` | table | no | If present, generate a stub instead of copying the host file. The stub is a JSON object with the specified fields, wrapped under the same top-level key structure as the original file. |

When `stub` is **present**: the host file is not copied. A generated stub JSON
is placed at the guest path. The host file is only consulted to verify the user
is actually logged in (it must exist and contain a non-empty `accessToken`).

When `stub` is **absent**: the host file is copied as-is. This is the fallback
for tools that don't support proxy-layer credential injection.

### Egress policy (`policies/default.toml`)

Extend `EgressRule` to support reading credentials from a JSON file:

```toml
[[egress]]
domain = "api.anthropic.com"
inject_header = "Authorization"
credential_file = "~/.claude/.credentials.json"
json_path = "claudeAiOauth.accessToken"
header_template = "Bearer {value}"
```

New fields (alternative to `env_var`):

| Field | Type | Description |
|-------|------|-------------|
| `credential_file` | string | Path to a JSON file on the host. Supports `~`. |
| `json_path` | string | Dot-separated path to extract the value (e.g., `claudeAiOauth.accessToken`). |

Either `env_var` or `credential_file` + `json_path` must be set, not both.

## Implementation Details

### 1. Config types (`config.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuestConfig {
    #[serde(default)]
    pub credential_files: Vec<CredentialFileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialFileEntry {
    pub host: String,
    pub guest: String,
    #[serde(default = "default_credential_mode")]
    pub mode: String,
    pub stub: Option<toml::Value>,
}
```

`AboxConfig` gains `#[serde(default)] pub guest: GuestConfig`.

### 2. Boot metadata (`boot_meta.rs`)

New struct:

```rust
pub struct StagedCredential {
    pub index: usize,
    pub guest_path: String,
    pub mode: String,
}
```

`BootMeta` gains `pub credential_files: Vec<StagedCredential>`.

`runner_script()` emits placement commands between env exports and exec:

```sh
mkdir -p '/.claude'
cp '/abox-meta/credentials/0' '/.claude/.credentials.json'
chmod 0600 '/.claude/.credentials.json'
```

All interpolated values pass through the existing `sh_escape()` function.

### 3. Orchestrator (`sandbox.rs`)

After `meta.stage(&meta_dir)`, for each `credential_files` entry:

1. Expand `~` in `host` path.
2. If the host file doesn't exist, log a debug warning and skip.
3. If `stub` is set:
   - Read the host file to verify the user is logged in (file exists). Skip
     with a warning if not.
   - Serialize the `stub` value directly as JSON. The stub in the TOML config
     must be the complete file content (the config author controls the
     structure). For Claude Code, the config would be:
     ```toml
     stub = { claudeAiOauth = { accessToken = "abox-proxy-managed",
              refreshToken = "abox-proxy-managed", expiresAt = 9999999999999,
              scopes = ["user:inference"], subscriptionType = "pro" } }
     ```
   - Write the stub to `<meta_dir>/credentials/<index>`.
4. If `stub` is not set:
   - Copy the host file to `<meta_dir>/credentials/<index>`.
5. Push a `StagedCredential` to `BootMeta`.

### 4. Policy engine (`policy.rs`)

Extend `EgressRule`:

```rust
pub struct EgressRule {
    pub domain: String,
    pub inject_header: String,
    pub env_var: Option<String>,           // existing
    pub credential_file: Option<String>,   // new
    pub json_path: Option<String>,         // new
    pub header_template: String,
}
```

### 5. Egress proxy (`egress_proxy.rs`)

In `handle_mitm_with_injection`, when resolving the credential value:

- If `env_var` is set: read from `std::env::var()` (existing behavior).
- If `credential_file` + `json_path` is set: read the JSON file, extract the
  value at the dot-separated path, use it as the credential.
- Cache the file read (the token doesn't change during a sandbox session).

### 6. Policy file update (`default.toml`)

Replace the existing `api.anthropic.com` rule:

```toml
[[egress]]
domain = "api.anthropic.com"
inject_header = "Authorization"
credential_file = "~/.claude/.credentials.json"
json_path = "claudeAiOauth.accessToken"
header_template = "Bearer {value}"
```

Keep the existing `env_var` rules for OpenAI, Google, GitHub as-is.

## Claude Code Auth Check Details

Verified by reading the Claude Code binary (v2.1.104). The local auth check:

1. `Aq()` reads `$9().read()?.claudeAiOauth` — needs `accessToken` non-null.
2. `aR(scopes)` checks `scopes.includes("user:inference")`.
3. `Eg(expiresAt)` returns true (expired) if `Date.now() + 300000 >= expiresAt`.
   Stub needs `expiresAt` far in the future.
4. `refreshToken` must be truthy (checked with `!K?.refreshToken`).

API requests go to `https://api.anthropic.com` with header
`Authorization: Bearer {accessToken}`.

No JWT decoding, no format validation, no pre-flight auth request.

## Security Properties

- **Credentials never enter the VM.** The stub contains only placeholder values.
- **Stub is worthless without the proxy.** `abox-proxy-managed` is not a valid
  token for any API.
- **Real credential stays on host.** Read by the proxy at request time from the
  host filesystem.
- **Per-sandbox attribution.** Each sandbox has its own proxy instance; all
  injected requests are audit-logged with the sandbox ID.
- **Policy-driven.** Only domains with explicit egress rules get credential
  injection. All other HTTPS traffic is denied or passed without modification.

## Testing

1. **Unit test:** `BootMeta::runner_script()` emits credential placement
   commands correctly.
2. **Unit test:** Policy engine parses `credential_file` + `json_path` rules.
3. **Unit test:** Egress proxy resolves credential from JSON file.
4. **Integration test:** Boot a sandbox with a stub, run
   `claude --print "say hello"`, verify it authenticates and responds.

## Future Extensions

- **Codex/other tools:** Add credential file entries with appropriate stubs.
  The mechanism is generic.
- **Token refresh:** If sandbox sessions exceed token lifetime, the proxy could
  refresh the token on the host side. Out of scope for now.
- **Credential file watching:** The proxy could watch the host file for changes
  (e.g., token refresh by the host's Claude Code). Out of scope for now.
