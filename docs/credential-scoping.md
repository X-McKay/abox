# Credential Scoping Guide

abox keeps real secrets on the **host** and uses managed boundaries to apply
them on the agent's behalf. That is a strong security improvement, but token
scope still matters: a prompt-injected agent can only do what the host-held
credential is allowed to do.

This guide covers the default supported auth surface:

- Claude Code
- Codex / OpenAI
- GitHub via managed `git` / constrained `gh`

See also [ADR-006](decisions/006-managed-auth-product-principles.md).

## How managed auth works

1. `abox init` or manual config enables a managed provider in
   `~/.abox/config.toml`.
2. abox stages a **stub** credential file into the guest.
3. The real credential stays on the host.
4. The host-side proxy or host-side CLI execution path applies the real
   credential at request time.

The guest sees only placeholder values such as `"abox-proxy-managed"`.

## GitHub

GitHub is a **host-managed workflow surface**, not a default API-egress
surface.

Out of the box, abox supports GitHub through:

- `git`
- constrained `gh`

That means your GitHub credential should be configured for host-side tooling,
not copied into the VM.

### Recommended credential shape

Use a **fine-grained personal access token** scoped to the specific repository
or repositories the agent needs.

Minimum useful permissions for a coding workflow:

- `Contents`: read and write
- `Pull requests`: read and write
- `Metadata`: read-only

Do **not** grant:

- `Administration`
- destructive org-level scopes
- repo deletion / webhook management
- any broader repository set than the agent actually needs

### Why scope still matters

abox constrains which `gh` subcommands are allowed by policy, but GitHub auth is
still powerful. If the token can write to many repositories, the agent can act
within those permissions through the allowed host-managed workflow.

## Anthropic (Claude Code)

Claude Code uses the host credential file at:

- `~/.claude/.credentials.json`

Enable it in `~/.abox/config.toml`:

```toml
[auth.providers.claude]
enabled = true
```

Optional override:

```toml
[auth.providers.claude]
enabled = true
host_credential_file = "/path/to/custom/.credentials.json"
```

### Scoping considerations

- The OAuth token is managed by Claude Code's own login flow.
- abox keeps that token on the host and stages only a stub into the guest.
- The account or org role behind the token still matters. Use a non-admin
  account for agent work where possible.

## Codex / OpenAI

Codex uses the host credential file at:

- `~/.codex/auth.json`

Enable it in `~/.abox/config.toml`:

```toml
[auth.providers.codex]
enabled = true
```

Optional override:

```toml
[auth.providers.codex]
enabled = true
host_credential_file = "/path/to/custom/auth.json"
```

For OpenAI-compatible HTTP clients, the default policy also supports the host
environment variable:

```bash
export OPENAI_API_KEY="sk-proj-..."
```

### Scoping considerations

- Prefer project-scoped keys when the OpenAI account model supports them.
- Set spending limits on the project or account used for agent work.
- Keep the repo/workflow boundary narrow; abox protects the secret location, not
  your upstream billing policy.

## Advanced Custom Stubs

Unsupported tools can still use an explicitly advanced, stub-only escape hatch.

Example:

```toml
[auth.advanced]
[[auth.advanced.stub_files]]
host_credential_file = "~/.tool/auth.json"
guest = "~/.tool/auth.json"
mode = "0600"

[auth.advanced.stub_files.stub]
access_token = "abox-proxy-managed"
refresh_token = "abox-proxy-managed"
```

If you use this path, you must also add the matching host-side policy yourself.
The advanced stub only satisfies the guest tool's local file check. It does not
automatically create the corresponding egress or CLI policy rule.

Use this path only when:

- the tool is not a first-class managed provider
- a stub is enough for local startup checks
- you understand the host-side policy you are enabling

## General Principles

1. Keep the credential on the host.
2. Use the narrowest upstream scope you can.
3. Prefer provider-specific managed auth over custom stubs.
4. Treat advanced custom stubs as power-user configuration, not the default
   product path.
5. Review `~/.abox/logs/audit.jsonl` after incidents or suspicious runs.

## Quick Reference

| Surface | Host source | Default path | Notes |
|---------|-------------|--------------|-------|
| Claude Code | OAuth file | `~/.claude/.credentials.json` | First-class managed provider |
| Codex | OAuth file | `~/.codex/auth.json` | First-class managed provider |
| OpenAI API | env var | `OPENAI_API_KEY` | Optional host-side alternate for HTTP clients |
| GitHub | host `git` / `gh` auth | host-managed | No default GitHub API egress injection |
