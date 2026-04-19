# Constrain Proxy Bridge Working Directories to the Sandbox Worktree

**Created:** 2026-04-18  
**Source:** `abox` repository review  
**Priority:** P0  
**Effort:** M  
**Severity:** High  
**Area:** isolation, credential proxy, host command execution  
**Related:** [`2026-04-08-vm-e2e-mvp-followups.md`](./2026-04-08-vm-e2e-mvp-followups.md), [`../decisions/004-non-root-guest-execution.md`](../decisions/004-non-root-guest-execution.md)

## Summary

The per-VM proxy bridge already does a component-aware `/workspace` prefix rewrite, but it still has two important boundary gaps:

- requests whose `cwd` does not map through `/workspace` are passed through unchanged in fixed-attribution mode; and
- requests that do map into the host worktree are not canonicalized or checked for final containment within that worktree.

A guest process that can talk to the proxy socket can therefore ask the host to run an allowed command from an arbitrary host directory instead of the sandbox worktree.

This is especially concerning because the guest init script makes `/run/abox-proxy.sock` world-writable (`chmod 666`), so any process inside the guest can connect to it.

## Why It Matters

`abox` relies on the host proxy to execute trusted host-side commands on behalf of the sandbox. If the guest can steer those commands to an unexpected host directory, then the worktree boundary stops being meaningful.

That creates several risks:

- Allowed commands like `git` may run against the wrong repository.
- The host proxy may read or mutate host state outside the intended worktree.
- Future policy additions could accidentally widen the blast radius further.

This is an isolation issue, not just a UX bug.

## Current Behavior

Today the bridge does this:

1. Parse the guest request.
2. If `cwd` is `/workspace` or starts with `/workspace/`, rewrite that prefix to the host worktree path.
3. Otherwise, leave `cwd` unchanged.
4. Pass the resulting path directly to `tokio::process::Command::current_dir`.

The component-aware prefix check is already doing the right thing for lookalikes such as `/workspacefoo`. The remaining holes are:

- fixed-attribution mode still accepts unmapped CWDs like `/tmp`;
- mapped paths are not canonicalized;
- there is no final containment check ensuring the resolved path stays inside the host worktree.

## Affected Code

- `crates/abox-core/src/proxy_bridge.rs`
- `guest/init.sh`

## Recommended Fix

Treat the worktree path as an enforced boundary in per-VM mode.

1. In `SandboxAttribution::Fixed` mode, reject unmapped CWDs and only accept guest CWDs rooted at `/workspace`.
2. Translate `/workspace/...` to the host worktree path and canonicalize the result.
3. Verify the canonicalized path is equal to, or contained within, the canonicalized host worktree root.
4. Reject requests whose CWD is missing, malformed, escapes via `..`, or resolves outside the worktree after translation.
5. Tighten the guest socket permissions so only the intended guest user can connect, unless there is a documented reason to keep the socket world-writable.

## Suggested Implementation Notes

- Add a helper dedicated to per-VM path translation and boundary checking.
- Make the helper return a structured denial reason so the audit log explains why a request was rejected.
- Keep the shared-daemon path behavior separate from the fixed-attribution path behavior. The two modes have different trust assumptions.
- Preserve the existing component-aware `/workspace` matching behavior; the bug is not in that specific prefix check.

## Acceptance Criteria

- A proxied command with `cwd="/workspace"` still succeeds.
- A proxied command with `cwd="/workspace/subdir"` still succeeds when that subdirectory exists.
- A proxied command with `cwd="/tmp"` is denied in per-VM mode.
- A proxied command with traversal like `"/workspace/../../.."` is denied.
- Tests cover both the path-boundary logic and the socket-permission decision.

## Validation Ideas

- Add unit tests around the CWD translation helper.
- Add a proxy bridge integration test that attempts to escape the worktree boundary.
- Add a VM-level test proving normal `git` operations still work from nested directories in the guest.
