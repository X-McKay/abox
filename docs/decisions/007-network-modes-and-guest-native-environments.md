# ADR-007: Network Modes and Guest-Native Repo Environments

**Date:** 2026-05-09
**Status:** Accepted

## Context

abox's primary value is a simple, secure sandbox for AI coding agents:

- the agent runs inside a microVM
- real secrets stay on the host
- host-side command and HTTPS boundaries remain mediated by abox

That security model is already established by the current architecture and
managed-auth decisions. The new product pressure comes from two directions:

1. Users need clearer outbound network control for different agent workflows:
   - safest-by-default coding sessions
   - targeted development sessions that need a small set of extra hosts
   - broad research sessions that need wide web access
2. Users need faster repo development workflows without reinstalling
   dependencies from scratch in every sandbox session.

An earlier prototype combined named network presets with project-specific
filesystem overlays exported from arbitrary Dockerfiles. That prototype showed
real demand, but it also mixed together several different concerns:

- secret handling
- host safety
- internet egress scope
- toolchain reuse
- package cache reuse
- repo-local build artifact reuse

This ADR separates those concerns and defines a simpler, more supportable
product surface for the next release.

## Decisions

### 1. The user-facing network model is modes, not raw presets

abox exposes three user-facing outbound network modes:

- `safe`
- `scoped`
- `open`

These are the primary product abstraction. They replace "choose or author an
arbitrary preset file" as the normal UX.

The security boundary does not change across modes:

- the guest still has no direct NIC-based outbound network path
- outbound web traffic still goes through abox's host-mediated proxy channel
- real secrets still stay on the host

What changes across modes is the allowed destination set.

### 2. `safe` is the default mode

`safe` is the default run mode and the recommended repo default.

In `safe` mode, only abox's built-in managed network surface is allowed by
default. Initially that surface is the narrow first-class set already aligned
with managed-auth product principles:

- Claude Code / Anthropic managed domains
- Codex / OpenAI managed domains

Everything else is denied by default.

### 3. `scoped` adds explicitly declared destinations through bundles and hostnames

`scoped` mode extends `safe` with repo- or run-specific additions.

Those additions are expressed through:

- curated named network bundles for common destination sets
- explicit hostname additions for targeted one-off needs

Examples of named bundles:

- `npm-public`
- `pypi-public`
- `cargo-public`

The simple scoped UX is intentionally limited to hostnames and bundles. It does
not expose low-level egress policy authoring, credential injection rules, raw
header mutation, or arbitrary path/method controls.

### 4. `open` means broad proxy-mediated HTTPS egress, not raw guest networking

`open` is an explicit lower-trust mode for research and general browsing
agents.

In `open` mode:

- broad outbound HTTPS/web API destinations are allowed through the abox proxy
  boundary
- managed domains still use host-held credential injection
- the host safety and host-secret boundary remain in place

In the initial implementation, `scoped` and `open` govern standard
CONNECT-mediated web traffic on the default TLS port (`443`). Additional ports
remain advanced, host-owned behavior rather than part of the simple repo UX.

`open` does **not** mean "the guest gets unrestricted raw network access."
abox remains the mediation boundary even in the most permissive mode.

### 5. Transport stays hybrid, but mode and bundle metadata drive it more explicitly

abox already has two transport behaviors:

- **MITM TLS termination** when abox must inject or apply a host-held secret
- **TLS passthrough** when no secret injection is needed

The new behavior is that network mode and bundle metadata make the choice
explicit and user-legible:

- managed credential-bearing domains use MITM
- public registries, docs sites, and general web access default to passthrough

This keeps MITM limited to the cases where it provides real product value and
preserves better compatibility for public network flows.

Passthrough traffic remains auditable at the destination-host level, not as a
fully parsed HTTP stream.

### 6. Host policy remains authoritative for managed integrations and advanced transport overrides

`~/.abox/policies/default.toml` remains part of the product:

- it continues to define built-in managed integrations
- it continues to own advanced host-managed egress rules
- it continues to own global transport overrides such as `bypass_tls`

Per-sandbox network modes are a user-facing policy layer on top of that host
surface:

- repo config may widen unmanaged destination scope in structured ways
- repo config may not define new credential injection behavior
- existing host-managed custom integrations remain supported, but must be
  surfaced explicitly in explanation and approval output

This keeps current advanced users supportable without making raw host policy
authoring the main repo UX.

### 7. Prompt input is added as a first-class input surface separate from sandbox identity

`--task` remains the sandbox/task identifier only.

Prompt content is a separate input surface and may come from:

- inline prompt text
- a prompt file
- a repo-declared default prompt file

The first release must define delivery all the way to the known managed agent
CLIs rather than merely staging a file:

- bare `claude` plus prompt input is rewritten into Claude's non-interactive
  print flow with the resolved prompt as the initial query
- bare `codex` plus prompt input is rewritten into `codex exec`, with the
  resolved prompt passed directly or via stdin
- arbitrary commands are not promised prompt delivery through this feature

Long prompts should be supported as first-class file-based input rather than
being forced into CLI inline text.

### 8. Repo environment reuse is guest-native and explicit

abox's first-class reusable environment model is **not** arbitrary filesystem
overlay from exported Docker images.

Instead, the initial supported reuse model is guest-native and explicit:

- managed per-project caches
- repo-defined prepare/bootstrap flows executed inside the real abox guest

Prepared templates or snapshots remain a possible optimization path, but they
are not part of the initial compatibility claim unless snapshot retargeting
against fresh worktrees and fresh abox metadata is proven supportable.

### 9. Broader ecosystem support should use official environment profiles, not repo-selected image paths

The validated base guest should remain small.

When abox needs stronger ecosystem support, the product should evolve through a
small set of official guest environment profiles selected by name in repo
config, not by exposing arbitrary guest image paths:

- `base`
- `node`
- `python`
- `rust`

The first official profile contract should be:

- `base`: common guest baseline only; no language-specific toolchain promise
- `node`: `base` plus `node` and `npm`
- `python`: `base` plus `python3`, `uv`, and `pip3`, with `uv` as the
  recommended default workflow
- `rust`: `base` plus `rustc` and `cargo`

One profile is selected per repo. Profile installation, image layout, and path
resolution remain host-owned concerns; repo config chooses a profile name, not
an image path.

This keeps the repo UX simple, keeps compatibility claims explicit, and avoids
turning `.abox/project.toml` into a low-level VM-image selection surface.

### 10. Managed caches and approval state have different reuse rules

Managed caches are long-lived optimization state. They are keyed per project
and ecosystem and invalidated rarely.

Approval state is stricter. It must include not just repo config bytes, but
also the versioned meaning of repo-declared bundles and referenced behavior
files, including:

- repo config
- network bundle catalog version or resolved host set
- referenced prompt file contents
- referenced prepare script contents

This avoids silent widening when bundle definitions change in a later abox
release.

### 11. Repo behavior lives in one primary repo config file

abox uses one primary repo-level config file:

- `.abox/project.toml`

This file may declare:

- repo/project identity override
- network mode
- scoped bundles and hostname additions
- environment profile
- environment caches
- prepare script path
- environment watch inputs
- optional default prompt file

Machine-specific and secret-adjacent settings remain in:

- `~/.abox/config.toml`

Per-run intent remains on the CLI.

### 12. Repo config is strictly validated and trust-on-first-use approved

Repo config is behaviorally significant: it can widen egress, select `open`,
define prepare flows, and select default prompt content.

Therefore:

- repo config is strictly validated
- unknown keys are treated as errors
- invalid or contradictory config fails closed
- repo config is not silently auto-trusted

abox uses trust-on-first-use with remembered approval for repo behavior.
When repo behavior changes materially, abox asks for approval again and stores
an approval fingerprint in host state.

For launch-time safety and legibility:

- repo prompt and prepare inputs must be staged from the approved bytes, not
  read live from the worktree after approval
- explanation output must show committed-content trust surfaces such as the
  default prompt file and prepare script together with network scope

## Consequences

### Positive

- The strongest security claim remains simple and defensible.
- Users get three understandable network shapes instead of raw preset authoring.
- Public network compatibility improves because MITM is only used where needed.
- Existing host-managed advanced integrations and TLS bypass rules can coexist
  with the new repo UX.
- Prompt-file support has a concrete, provider-aware contract instead of a
  staging-only placeholder.
- Repo environment reuse is based on the real guest runtime rather than
  best-effort foreign filesystem overlays.
- A small official profile set provides a clearer ecosystem contract than a
  single ever-growing guest image.
- The repo config surface becomes easier for both humans and AI tools to use.
- Config review and trust become explicit rather than implicit.

### Negative / Trade-offs

- The product gives up arbitrary Dockerfile-overlay flexibility as a
  first-class default story.
- A maintained network-bundle catalog is required for `scoped` mode.
- `open` still requires careful operator understanding: host secrets remain
  protected, but internet exfiltration risk is intentionally wider.
- The initial `open` / `scoped` contract is intentionally limited to standard
  HTTPS egress rather than arbitrary ports and protocols.
- Trust-on-first-use adds an approval step to repo-driven behavior.
- Prompt delivery is intentionally first-class only for known managed agent
  launch paths, not arbitrary guest commands.
- Full warm-template reuse is deferred until restore retargeting is proven.
- Maintaining `base`, `node`, `python`, and `rust` as official profiles adds a
  real image-build and validation matrix.

## What This ADR Deliberately Does Not Do

- It does not define a raw guest networking mode.
- It does not expose arbitrary custom credential injection through repo config.
- It does not make method/path-level egress control part of the simple UX.
- It does not standardize generic Docker-exported rootfs overlay as a supported
  reuse contract.
- It does not expose arbitrary repo-selected guest image paths as the normal
  UX.
- It does not commit to arbitrary profile composition in repo config.
- It does not claim snapshot-backed prepared-environment reuse before the
  restore model is proven against fresh worktrees and fresh metadata.
