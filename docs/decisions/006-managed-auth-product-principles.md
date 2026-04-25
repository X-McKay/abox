# ADR-006: Managed Authentication Product Principles

**Status:** Accepted
**Date:** 2026-04-24

## Context

abox was converging on a useful security property — proxy-managed credentials
for supported tools — but parts of the shipped product still exposed an older,
file-staging-oriented mental model. That mismatch showed up in three places:

1. The primary configuration surface still looked like "stage credential files
   into the guest", even though the secure path is host-held secrets plus guest
   stubs.
2. The default policy surface was broader than the product we actually want to
   support out of the box.
3. Public docs sometimes described the secure property too absolutely without
   distinguishing managed defaults from advanced user extensions.

This ADR defines the product boundary going forward.

## Decisions

### 1. Real secrets never enter the VM

abox no longer treats "copy a host credential file into the guest" as a
supported configuration path.

Supported auth inside the guest is **stub-only**:

- the guest may receive a placeholder credential-shaped file
- the real credential remains on the host
- the host injects or applies it at the managed boundary

If a workflow cannot be expressed this way, it is not a first-class supported
auth pattern until abox adds a managed integration for it.

### 2. Default first-class auth surface is intentionally narrow

Out-of-the-box managed support is limited to:

- **Claude Code**
- **Codex**
- **GitHub via host-managed `git` + constrained `gh`**

Everything else is outside the default product surface unless explicitly added
later as a managed integration.

Notably:

- **Google** is not part of the default product surface.
- **AWS** is not part of the default product surface in this release train.
- Default GitHub support is a **managed CLI workflow**, not broad GitHub API
  egress.

### 3. Provider-first configuration is the primary UX

The main user-facing config model is provider-first, not file-first.

Users should enable managed providers such as Claude and Codex directly.
abox owns the stub format, guest path, and host lookup convention for those
first-class providers.

This keeps the primary UX aligned with the real security boundary:

- enable provider
- keep secret on host
- let abox manage the guest stub + host-side injection

### 4. Advanced custom stubs remain available, but clearly separated

abox keeps an advanced escape hatch for unsupported tools, but it is:

- **separate from the main managed-provider UX**
- **stub-only**
- **documented as advanced**

Users who adopt this path are responsible for supplying the matching host-side
policy and understanding the residual risk.

### 5. GitHub is a managed workflow surface, not a generic API surface

Default GitHub support is provided through:

- managed `git`
- constrained `gh`

The default `gh` surface is limited to:

- `gh pr list`
- `gh pr view`
- `gh pr diff`
- `gh pr create`
- `gh issue list`
- `gh issue view`
- `gh repo view`
- `gh auth status`

It intentionally excludes higher-risk or unnecessary control-plane actions such
as merge, checkout-driven state changes, issue creation, auth mutation, and repo
deletion.

Default broad `api.github.com` credential injection is removed.

### 6. The default security claim must stay precise

abox may claim the following for its default supported flows:

- real secrets stay on the host
- default-supported tools use managed outbound channels
- guest-visible credential files are worthless stubs

abox does **not** currently claim full method/path-level least privilege for
egress-managed providers. Today, least privilege still depends partly on:

- provider-side token scope
- the domain-level policy boundary

That is a meaningful security improvement, but it is not the same thing as a
fully granular API authorization layer.

## Consequences

### Positive

- The strongest product guarantee becomes simple and defensible.
- The main config surface matches the actual trust boundary.
- The default policy surface is smaller and easier to reason about.
- GitHub support becomes clearer: host-managed workflow support, not generic API
  exposure.

### Negative / Tradeoffs

- Some formerly possible ad hoc workflows are no longer default-supported.
- Users with niche provider requirements must use the advanced stub mechanism or
  wait for a managed integration.
- Documentation and onboarding must become more explicit, because the advanced
  path is still present but intentionally de-emphasized.

## Immediate Implementation Direction

The codebase should align with this ADR by:

1. removing copy-mode credential staging
2. making Claude and Codex explicit managed providers in config and init flows
3. trimming the default policy to the agreed first-class surface
4. warning loudly when managed providers are enabled but host credentials are
   missing
5. documenting the advanced custom-stub path separately from the default UX
