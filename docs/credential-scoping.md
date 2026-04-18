# Credential scoping guide

abox injects credentials into agent requests at the network layer — the agent never sees real tokens. But the *scope* of those tokens matters: a GitHub token with `admin:org` permission is just as dangerous when injected by a proxy as when read from disk, because a rogue agent controls what API calls it makes on allowed domains.

This guide covers how to create minimally-scoped credentials for each provider abox supports.

---

## How credential injection works

Quick recap (see [`docs/explainer.md`](explainer.md) sections 6–7 for the full story):

1. The agent inside the VM sees only **stub credentials** — placeholder files with values like `"abox-proxy-managed"`.
2. When the agent makes an HTTPS request, abox's egress proxy intercepts it.
3. The proxy evaluates the destination against your **policy file** (`~/.abox/policies/default.toml`).
4. If the domain matches an egress rule with credential injection, the proxy reads the real token from your **host environment** (env var or file) and injects it into the `Authorization` header.
5. The request reaches the upstream API with real credentials. The agent never sees them.

**The risk:** the proxy injects credentials but does not filter what the agent *does* with those credentials. If your GitHub token can delete repositories, a prompt-injected agent can call `DELETE /repos/{owner}/{repo}` on `api.github.com` — the proxy will happily attach your token to that request.

**The mitigation:** use the narrowest possible token scope for each provider.

---

## Provider-specific guidance

### GitHub

abox injects `GITHUB_TOKEN` as a Bearer token on requests to `api.github.com`. This is also used by the `gh` CLI when proxied through the CLI policy engine.

**Recommended: fine-grained personal access token (PAT)**

Create one at [github.com/settings/personal-access-tokens/new](https://github.com/settings/personal-access-tokens/new):

| Setting | Value |
|---------|-------|
| **Resource owner** | Your personal account or the org that owns the repo |
| **Repository access** | "Only select repositories" — pick the repo(s) agents will work on |
| **Permissions** | See table below |

**Minimum permissions for a coding agent:**

| Permission | Access | Why |
|------------|--------|-----|
| Contents | Read and write | Read/write files, create commits, push branches |
| Pull requests | Read and write | Create and update PRs |
| Metadata | Read-only | Required by GitHub for all fine-grained PATs |

**Do NOT grant** unless you have a specific reason:

| Permission | Risk |
|------------|------|
| Administration | Can delete the repo, change settings, manage webhooks |
| Actions | Can trigger/cancel CI workflows |
| Environments, Secrets | Can read/write deployment secrets |
| Organization permissions | Grants access beyond the selected repo |

**Set the token:**

```bash
export GITHUB_TOKEN="github_pat_..."
```

Or add it to your shell profile so abox picks it up at runtime.

**Classic PATs:** If you must use a classic PAT, select only the `repo` scope. Never select `admin:org`, `delete_repo`, or `admin:repo_hook`. Classic PATs cannot be scoped to a single repository — prefer fine-grained PATs.

---

### Anthropic (Claude Code)

abox reads the Claude OAuth token from `~/.claude/.credentials.json` at the JSON path `claudeAiOauth.accessToken`. This token is managed by Claude Code's own login flow — you do not create it manually.

**Scoping considerations:**

- The token's scope is determined by your Claude subscription and the OAuth consent flow. You cannot narrow it further.
- The token grants inference access (sending prompts, receiving completions). It does not grant account management, billing changes, or API key creation.
- If you use a team/organization Claude account, the token inherits your role's permissions. Ensure the account used for agent work does not have admin privileges on the organization.

**Stub configuration** (already in the default `config.example.toml`):

```toml
[[guest.credential_files]]
host = "~/.claude/.credentials.json"
guest = "~/.claude/.credentials.json"
mode = "0600"

[guest.credential_files.stub.claudeAiOauth]
accessToken = "abox-proxy-managed"
refreshToken = "abox-proxy-managed"
expiresAt = 9999999999999
scopes = ["user:inference"]
subscriptionType = "pro"
```

The stub satisfies Claude Code's startup checks. The real token is injected by the proxy.

---

### OpenAI (Codex)

abox supports two credential sources for OpenAI, checked in this order:

1. **Environment variable** `OPENAI_API_KEY` — checked first
2. **Credential file** `~/.codex/auth.json` at path `tokens.access_token` — fallback

**If using an API key (`OPENAI_API_KEY`):**

Create one at [platform.openai.com/api-keys](https://platform.openai.com/api-keys):

- Use a **project-scoped key** (not a user-level key) if your OpenAI organization supports projects
- Set a **spending limit** on the project to cap damage from a runaway agent
- If possible, restrict the key to only the models the agent needs (e.g., `gpt-4o` only)

```bash
export OPENAI_API_KEY="sk-proj-..."
```

**If using Codex OAuth (`~/.codex/auth.json`):**

The token is managed by Codex's login flow. Scoping is determined by your OpenAI account role. The stub:

```toml
[[guest.credential_files]]
host = "~/.codex/auth.json"
guest = "~/.codex/auth.json"
mode = "0600"

[guest.credential_files.stub]
auth_mode = "chatgpt"

[guest.credential_files.stub.tokens]
id_token = "abox-proxy-managed"
access_token = "abox-proxy-managed"
refresh_token = "abox-proxy-managed"
account_id = "abox-proxy-managed"
last_refresh = "2099-01-01T00:00:00Z"
```

---

### Google (googleapis.com)

abox injects `GOOGLE_API_KEY` as a Bearer token on requests to `*.googleapis.com`.

**Recommended: API key with restrictions**

Create one at [console.cloud.google.com/apis/credentials](https://console.cloud.google.com/apis/credentials):

- **Application restrictions:** Set to "None" (or IP-restrict to your machine if feasible)
- **API restrictions:** Select only the specific APIs the agent needs (e.g., Vertex AI API). Do not leave as "Unrestricted."

```bash
export GOOGLE_API_KEY="AIza..."
```

For service accounts: create a dedicated service account with only the IAM roles the agent needs. Never use your personal account's credentials.

---

### AWS (CLI commands)

AWS credentials are used by the CLI proxy (not the egress proxy) when the agent runs `aws` commands. The policy engine controls which `aws` subcommands are allowed.

**Recommended: scoped IAM credentials**

- Create a dedicated IAM user or role for agent work
- Attach a policy that grants only the permissions the agent needs
- Use short-lived credentials via `aws sts assume-role` with a session duration cap

**Example: agent that only needs S3 read and CloudWatch log access:**

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": ["s3:GetObject", "s3:ListBucket"],
      "Resource": "arn:aws:s3:::my-bucket/*"
    },
    {
      "Effect": "Allow",
      "Action": ["logs:GetLogEvents", "logs:DescribeLogGroups"],
      "Resource": "*"
    }
  ]
}
```

The default policy already denies most AWS CLI subcommands — only `s3`, `sts get-caller-identity`, and `logs` commands are allowed.

---

## General principles

1. **Scope to the repo, not the account.** Fine-grained PATs (GitHub) and project-scoped keys (OpenAI) exist for this reason. Use them.

2. **Set spending limits.** For API providers that bill per-token (OpenAI, Anthropic, Google), set budget caps on the project or account the agent uses.

3. **Rotate after incidents.** If an agent behaves unexpectedly, rotate the credential immediately. Check `~/.abox/logs/audit.jsonl` to see what requests were made.

4. **Audit what you grant.** Run `abox doctor` — future versions will check token scopes where the provider API exposes them (e.g., GitHub's `X-OAuth-Scopes` response header).

5. **Prefer env vars over credential files** when the provider supports API keys. Env vars are never written to disk in any form — they exist only in the host process's memory and are injected per-request.

---

## Quick reference

| Provider | Source | Config key | Recommended scope |
|----------|--------|------------|-------------------|
| GitHub | env var | `GITHUB_TOKEN` | Fine-grained PAT: `contents:rw`, `pull_requests:rw`, single repo |
| Anthropic | file | `~/.claude/.credentials.json` | Managed by Claude login (inference only) |
| OpenAI | env var or file | `OPENAI_API_KEY` or `~/.codex/auth.json` | Project-scoped key with spending limit |
| Google | env var | `GOOGLE_API_KEY` | API-restricted key, specific APIs only |
| AWS | env var | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` | Scoped IAM role, short-lived session |
