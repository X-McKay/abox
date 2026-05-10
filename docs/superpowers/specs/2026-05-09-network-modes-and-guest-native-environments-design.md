# Network Modes and Guest-Native Repo Environments Design

## Context

abox already has a strong security and isolation story:

- microVM sandboxing
- host-held real secrets
- host-mediated CLI and HTTPS boundaries

What it lacks is:

- a user-friendly way to express outbound network intent
- a concrete story for long prompts
- a credible, supportable answer to the "I have to reinstall dependencies in
  every session" problem

This spec defines a release-oriented design that is:

- understandable by humans
- easy for AI tools to inspect and modify
- explicit about trust and risk
- compatible with the existing abox architecture

It intentionally avoids promoting arbitrary Docker-exported filesystem overlay
into the main supported product contract.

## Goals

1. Make the common network UX simple: safest-by-default, targeted additions,
   or broad research access.
2. Preserve the strongest current security claim: real secrets stay on the
   host in every supported mode.
3. Keep outbound web traffic behind abox mediation, even in permissive modes.
4. Keep the repo config surface narrow, explicit, and reviewable.
5. Reduce repeated dependency-install cost without pretending one mechanism
   solves every kind of environment reuse.
6. Broaden ecosystem support through a small official profile set rather than
   raw guest-image selection.

## Non-Goals

- Raw unrestricted guest networking
- Generic low-level policy authoring as the main UX
- Arbitrary custom credential injection in repo config
- Path/method-level egress policy in the simple UX
- Generic arbitrary Docker overlay as the first-class environment contract
- Claiming snapshot-backed warm reuse before restore retargeting is proven

---

## 1. User-Facing Network Model

The primary network abstraction is three modes:

- `safe`
- `scoped`
- `open`

### 1.1 `safe`

`safe` is the default mode.

Allowed destinations:

- built-in managed Anthropic domains
- built-in managed OpenAI / Codex domains
- host-owned advanced managed integrations, if present, surfaced explicitly as
  advanced host policy

Behavior:

- credential-bearing managed destinations use MITM injection
- all other outbound web traffic is denied

### 1.2 `scoped`

`scoped` extends `safe` with explicitly declared additions.

Allowed additions are expressed as:

- curated named network bundles
- explicit hostnames

Examples:

- `npm-public`
- `pypi-public`
- `cargo-public`
- `docs.rs`
- `developer.mozilla.org`

Behavior:

- managed destinations use MITM
- public / unmanaged additions use TLS passthrough by default
- everything not explicitly allowed remains denied

If `scoped` is configured with no bundles and no domains, abox normalizes it to
`safe` in the resolved run config and explains that normalization.

### 1.3 `open`

`open` is the explicit lower-trust mode for general browsing and research.

Behavior:

- broad outbound HTTPS/web API access is allowed through the abox proxy
  boundary
- managed destinations still use MITM
- non-managed destinations use passthrough by default

Important:

- `open` does not bypass the abox mediation boundary
- `open` widens internet exfiltration risk, not host secret exposure
- `open` is intentionally about standard web traffic, not arbitrary raw
  sockets or arbitrary protocols

### 1.4 Initial port and protocol contract

The first release keeps the contract intentionally narrow:

- allow HTTPS / TLS-wrapped web API traffic over the existing CONNECT path
- treat destination port `443` as the standard supported path
- keep non-standard ports and non-web protocols out of the simple repo UX

This is a user-experience choice as much as a security one. The product should
say "web access" and mean something supportable, not implicitly promise a
generic tunnel for any host:port pair.

### 1.5 Transport rules

All modes share the same top-level transport invariant:

- the guest does not get direct NIC-based outbound networking
- outbound web traffic remains mediated by abox

abox already has a hybrid transport path. The change in this design is that the
choice becomes mode- and bundle-driven and easier to explain:

- **MITM** for destinations where abox must inject or apply host-held
  credentials
- **passthrough** for destinations where no injection is needed

### 1.6 Audit expectations

Passthrough traffic remains auditable at the destination-host level:

- sandbox id
- destination host
- allow / deny result

It is not promised as full parsed HTTP request logging. Operator-facing docs
should say this plainly.

---

## 2. Host Policy Coexistence and Migration

The new repo UX does not replace `~/.abox/policies/default.toml`.

### 2.1 What the host policy still owns

The host policy remains authoritative for:

- built-in managed integrations
- advanced host-managed custom egress rules
- global TLS transport overrides such as `bypass_tls`
- CLI command policy

### 2.2 What repo config owns

Repo config owns:

- per-sandbox mode selection
- structured widening of unmanaged destination scope
- repo-local prepare and prompt defaults

Repo config may not define:

- new credential injection behavior
- raw header mutation rules
- custom host secret sources

### 2.3 Final decision model

The effective egress decision should be resolved like this:

1. Determine whether the destination is a host-managed integration.
2. Determine whether the sandbox mode allows the destination.
3. Determine transport:
   - host-managed rules and host `bypass_tls` remain authoritative
   - otherwise use the default transport defined by the mode / bundle / domain

This keeps advanced current users working without forcing raw host policy into
repo-owned config.

### 2.4 Existing user impact

First-run behavior after upgrade must be explicit:

- existing `bypass_tls` entries continue to apply
- existing host-owned advanced managed domains remain reachable
- `project explain` / approval output must surface that the effective `safe`
  surface is broader than the builtin default if advanced host policy is
  present

Migration does **not** require rewriting or deleting an existing
`~/.abox/policies/default.toml`.

---

## 3. Repo Config Surface

The primary repo behavior file is:

- `.abox/project.toml`

This file is intentionally narrow and declarative.

### 3.1 Proposed schema

Starter config generated by `abox project init` should stay minimal:

```toml
[network]
mode = "safe"
```

Expanded example:

```toml
[network]
mode = "scoped" # safe | scoped | open
bundles = ["npm-public"]
domains = ["docs.rs"]

[environment]
profile = "node" # base | node | python | rust
caches = ["npm"]
prepare = ".abox/prepare.sh"

[agent]
default_prompt_file = ".abox/prompt.md"
```

Optional advanced fields can be added later, for example:

```toml
[project]
id = "acme-webapp"

[environment]
watch = ["package-lock.json", "Cargo.lock"]
```

### 3.2 What does NOT belong in repo config

The repo config must not expose:

- host credential locations
- host paths outside the repo
- custom credential injection rules
- raw proxy header mutation
- low-level HTTP policy tables
- host-executed hooks

These remain host-owned or advanced-only concerns.

### 3.3 Precedence rules

Precedence is:

- CLI
- repo config
- host defaults
- built-in defaults

Examples:

- `--network open` overrides repo `network.mode`
- `--prompt-file` overrides repo `agent.default_prompt_file`
- host config never silently overrides repo network intent

---

## 4. Scoped Mode Expression

`scoped` mode supports both:

- curated bundles
- explicit hostname additions

### 4.1 Bundles are the primary path

Bundles are the common-case, validated building blocks.

Initial examples:

- `npm-public`
- `pypi-public`
- `cargo-public`

Each bundle compiles to:

- one or more allowed hostnames
- default transport behavior
- product-owned metadata and description
- a catalog version used by trust and explanation output

### 4.2 Hostname additions are the escape hatch

`domains = [...]` exists for targeted one-off needs not covered by a built-in
bundle.

The simple config accepts hostnames, not full URLs.

Valid:

- `docs.rs`
- `api.example.com`

Invalid in the simple config:

- `https://docs.rs/crate/tokio`
- `/api/v1`
- query-string fragments

This keeps the contract honest because passthrough destinations are enforced at
the host level, not as path-level policy in the simple UX.

### 4.3 No custom managed integrations in scoped config

Scoped config may add unmanaged destinations, but it may not define:

- new secret-bearing integrations
- credential sources
- header injection rules

If a new managed integration is needed, it must land as:

- an advanced host-owned policy
- or a future first-class managed provider or bundle

---

## 5. Prompt Input Model

Prompt content is added as a first-class abox input separate from sandbox
identity.

### 5.1 Prompt sources

Support:

- inline prompt text
- prompt file
- repo default prompt file
- optional stdin as a later generic UX

Examples:

```bash
abox run --task fix-auth --prompt "Fix the auth bug and add tests" -- codex
abox run --task fix-auth --prompt-file prompts/fix-auth.md -- codex
```

Repo default:

```toml
[agent]
default_prompt_file = ".abox/prompt.md"
```

### 5.2 Host-side resolution

If prompt input is specified, abox resolves it on the host before VM launch.

That resolved prompt is:

- staged into boot metadata for inspection and reproducibility
- included in approval fingerprinting
- handed off to supported managed-agent CLIs through an explicit launch adapter

### 5.3 First-release delivery contract

The first release must be explicit here.

When prompt input is used with a known managed agent:

- bare `claude` is rewritten into Claude's non-interactive print/query mode,
  with the resolved prompt passed as the initial query
- bare `codex` is rewritten into `codex exec`, with the resolved prompt passed
  directly or via stdin

When prompt input is used with an arbitrary command that abox does not know how
to adapt, abox must fail with a clear error instead of staging a file that no
process reads.

### 5.4 Why this is intentionally narrow

This design is narrower than "make prompt available somewhere in the guest" but
much more user-friendly:

- users get a real end-to-end feature for known managed agents
- reviewers do not have to guess who consumes the prompt
- the release does not claim a generic feature that is only half implemented

---

## 6. Repo Environment Reuse Model

The supported repo environment model is guest-native and explicit.

### 6.1 Official environment profiles are the simple path for broader ecosystem support

The product should not keep growing one baseline guest image indefinitely, and
it should not expose arbitrary guest image paths in repo config.

The follow-on environment model should instead use a small set of official
profiles selected by name:

- `base`
- `node`
- `python`
- `rust`

Repo config selects a profile name, not an image path:

```toml
[environment]
profile = "python"
caches = ["uv"]
prepare = ".abox/prepare.sh"
```

If `environment.profile` is omitted, it defaults to `base`.

The initial official profile contract should be:

- `base`: common guest baseline only; no language-specific toolchain guarantee
- `node`: `base` plus `node` and `npm`
- `python`: `base` plus `python3`, `uv`, and `pip3`; `uv` is the preferred
  first-class workflow
- `rust`: `base` plus `rustc` and `cargo`

Validated on the current release-prep branch:

- `node`: `node 20.15.1`, `npm 10.2.5`
- `python`: `Python 3.11.14`, `uv 0.11.12`, `pip 23.3.1`
- `rust`: `rustc 1.76.0`, `cargo 1.76.0`

This keeps user choice simple:

- pick the repo's primary ecosystem
- use the matching official profile
- let `prepare.sh` handle repo-specific setup on top

One profile is selected per repo in the first version. Profile composition and
repo-selected image paths stay out of the simple UX.

### 6.2 Initial supported primitives

The initial reuse model addresses repeated setup cost through two explicit
primitives:

1. **Managed caches**
2. **Prepare/bootstrap flows executed inside the real guest**

These are the first release compatibility claim.

### 6.3 Managed caches

Managed caches target package download and registry reuse, not general
workspace-local artifact sharing.

Examples:

- npm cache
- pip / uv wheel or download cache
- cargo registry and git cache

Managed caches are:

- per-project
- long-lived
- rarely invalidated
- visible in status and prune output

Profiles and caches should validate together:

- `node` is the supported profile for `npm`
- `python` is the supported profile for `uv` and `pip`
- `rust` is the supported profile for `cargo`

`base` should not silently imply language-specific cache support. If a repo
configures a cache family that is not supported by the selected profile, abox
should fail validation with a clear fix suggestion.

### 6.4 Prepare/bootstrap flow

The repo may declare a prepare script:

```toml
[environment]
prepare = ".abox/prepare.sh"
```

This script runs inside the real abox guest with the selected network mode and
managed caches available.

That makes the compatibility contract explicit:

- if prepare succeeds in the real guest, the workflow is guest-compatible
- if the selected profile does not provide the required toolchain, abox should
  fail fast before or at warm time with a profile-specific error

Two implementation details are worth making explicit in user-facing docs:

- Python repos should prefer a virtualenv-based `uv` workflow over
  `uv pip install --system`, because the guest Python is intentionally
  externally managed.
- The current `rust` profile toolchain (`rustc/cargo 1.76.0`) does not support
  Cargo edition 2024 or Cargo.lock version 4. The implementation should fail
  validation early with a clear message instead of discovering that only after
  boot.

### 6.5 What the first release does not promise

The first release does not promise generic safe reuse of all workspace-local
install trees across all tasks and ecosystems.

Examples:

- `node_modules`
- `.venv`
- `target/`

The first release should be honest that these may still be task-local even when
caches reduce overall setup cost significantly.

### 6.6 Snapshot-backed warm reuse is a follow-up optimization

Prepared templates / snapshots remain a valid direction, but they are not part
of the initial compatibility claim unless a spike proves that a restored VM can
be retargeted cleanly to:

- a fresh worktree
- fresh boot metadata
- fresh status reporting paths

Until that spike succeeds, the product should describe templates as an optional
future optimization rather than a guaranteed reuse mechanism.

---

## 7. Identity, Fingerprints, and Invalidation

### 7.1 Project identity

abox derives project identity automatically:

- preferred: normalized git remote URL
- fallback: canonical repo root path

Optional override:

```toml
[project]
id = "acme-webapp"
```

If multiple local clones intentionally share the same git remote identity, they
also share project-level caches unless the repo overrides the id.

### 7.2 Cache identity

Managed caches are keyed by:

- project identity
- ecosystem/cache type

Examples:

- `project_key + npm`
- `project_key + cargo`

They are invalidated minimally:

- explicit user reset
- corruption
- schema/version break

They are not invalidated by routine source edits or lockfile churn.

### 7.3 Approval fingerprint

Approval fingerprinting must include behavior, not just file paths.

At minimum:

- repo config contents
- network bundle catalog version, or the resolved bundle host set
- referenced prompt file contents
- referenced prepare script contents

This avoids silent scope changes after an abox upgrade.

### 7.4 Environment-relevant fingerprint inputs

Environment preparation logic should fingerprint:

- selected environment profile
- guest/rootfs version
- resolved repo config
- selected cache set
- prepare script contents
- relevant dependency lockfiles
- explicit `watch` files

Ordinary source file edits do not invalidate cache state.

### 7.5 Stage approved bytes, not live worktree paths

At launch time, abox should stage immutable copies of:

- the approved prompt content
- the approved prepare script

This prevents within-session worktree edits from changing what the user already
approved.

---

## 8. Trust and Validation Model

Repo config is behaviorally significant and must not be silently auto-trusted.

### 8.1 Trust-on-first-use

On first use, or when the repo behavior fingerprint changes materially, abox
shows a behavior summary and requests approval.

Example summary:

- network mode: `scoped`
- bundles: `npm-public`
- explicit domains: `docs.rs`
- advanced host-managed domains: `registry.internal.example.com`
- prepare script: `.abox/prepare.sh`
- default prompt file: `.abox/prompt.md`

Approval is stored in host state against the current behavior fingerprint.

### 8.2 Strict validation

Repo config must fail closed on:

- invalid TOML
- unknown keys
- invalid enum values
- unknown bundle names
- malformed domains
- missing referenced prompt file
- missing referenced prepare script
- contradictory settings

Examples of contradictory config:

- `mode = "safe"` with non-empty `bundles` or `domains`
- prompt input configured for a launch path abox cannot adapt

### 8.3 Normalization and warnings

Warnings, not failures:

- `scoped` mode with no additions, normalized to `safe`
- valid but unused watch files
- configured caches for ecosystems not detected yet
- advanced host policy broadening the effective safe surface

---

## 9. CLI UX

The simple config and environment model need first-class CLI support.

### 9.1 Keep one primary config command family

- `abox project init`
- `abox project validate`
- `abox project trust`
- `abox project explain`
- `abox project set-profile <base|node|python|rust>`
- `abox project add-bundle <bundle>`
- `abox project add-domain <hostname>`

`project explain` should summarize effective behavior in plain language.

The intent is:

- `project` owns repo config lifecycle
- `env` owns cache / prepare lifecycle
- `run` owns one sandbox launch

That keeps the surface compact and predictable.

### 9.2 Environment lifecycle

- `abox env status`
- `abox env warm`
- `abox env reset`
- `abox env prune`

`env status` and `env prune` should surface cache sizes and pruning impact so
long-lived caches stay legible rather than silently growing forever.

### 9.3 Happy path

The normal user flow should fit in a few commands:

1. `abox project init`
2. optionally `abox project set-profile node`
3. optionally `abox project add-bundle npm-public`
4. optionally `abox project add-domain docs.rs`
5. `abox project trust`
6. optionally `abox env warm`
7. `abox run --task fix-auth --prompt-file prompts/fix-auth.md -- codex`

Anything beyond that is an advanced path and should not be required for normal
repo development.

---

## 10. Release Scope Guidance

The release should treat these features as first-class:

- network modes (`safe`, `scoped`, `open`)
- scoped config with bundles and hostnames
- explicit coexistence with current host policy
- hybrid MITM/passthrough transport driven by mode metadata
- prompt-file support with concrete delivery for known managed agents
- guest-native caches and prepare flows
- repo trust/validation

The next expansion path after that validated foundation should be:

- official guest environment profiles (`base`, `node`, `python`, `rust`)
- profile-aware cache validation and toolchain preflight
- host-managed profile installation and visibility

The release should defer these from the first-class supported product contract:

- arbitrary Dockerfile-exported rootfs overlays
- generic low-level repo-authored policy files as the primary UX
- raw guest networking
- custom secret-bearing integrations through repo config
- arbitrary repo-selected guest image paths
- arbitrary profile composition
- snapshot-backed prepared-environment reuse until restore retargeting is
  proven

## Summary

The design intentionally favors:

- explicitness over magic
- supportable compatibility over maximal flexibility
- narrow default trust over silent broadening
- user- and AI-friendly structure over raw low-level power

That trade is consistent with abox's security model and makes the next release
easier to understand, easier to adopt, and easier to trust.
