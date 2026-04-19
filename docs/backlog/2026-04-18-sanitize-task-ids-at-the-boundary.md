# Sanitize and Validate Task IDs at the CLI Boundary

**Created:** 2026-04-18  
**Source:** `abox` repository review  
**Priority:** P1  
**Effort:** M  
**Severity:** Medium  
**Area:** naming, filesystem safety, git refs, runtime paths  
**Related:** [`2026-04-08-vm-e2e-mvp-followups.md`](./2026-04-08-vm-e2e-mvp-followups.md), [`2026-04-18-close-early-failure-state-leaks.md`](./2026-04-18-close-early-failure-state-leaks.md)

## Summary

Task IDs are currently passed through as raw user input even though the codebase already has a `sanitize_task_id()` helper. Those raw IDs are used in branch names, worktree paths, runtime socket names, console log names, and metadata directories.

Inputs containing `/`, whitespace, or other special characters can therefore create nested paths, invalid refs, or difficult-to-debug runtime failures.

## Why It Matters

Task IDs are foundational identifiers in `abox`. If they are not normalized up front, many later systems inherit the ambiguity:

- git branch creation may fail or create surprising names;
- Unix socket path construction becomes harder to reason about;
- filesystem layout may no longer be one task equals one directory;
- logs and cleanup behavior can become inconsistent.

This is partly a correctness issue and partly a hygiene issue.

## Current Behavior

The CLI passes `args.task` directly into `CreateSandboxParams`, and downstream code uses it as-is.

There is a sanitizer in `crates/abox-core/src/util.rs`, but it is not wired into the run path or consistently enforced elsewhere.

## Affected Code

- `crates/abox-cli/src/commands/run.rs`
- `crates/abox-core/src/util.rs`
- `crates/abox-core/src/adapters/cloud_hypervisor.rs`
- `crates/abox-core/src/adapters/git2_workspace.rs`

## Recommended Fix

Decide whether task IDs should be normalized or strictly validated, then enforce that rule once at the boundary.

Recommended approach:

1. Define the allowed task ID grammar explicitly.
2. Reject unsafe input with a clear error instead of silently mutating it, unless the team strongly prefers normalization.
3. If normalization is kept, surface the normalized ID to the user so branch names and runtime paths are predictable.
4. Use the validated form consistently across every subsystem.

## Suggested Implementation Notes

- Treat task IDs as an external interface, not an internal convenience string.
- If backward compatibility matters, add tests for previously used task ID shapes.
- Consider length checks as part of the same validation so runtime socket path limits are easier to enforce.

## Acceptance Criteria

- Invalid task IDs are rejected or normalized consistently before any side effects occur.
- Worktree, branch, and runtime-path generation all use the same canonical task ID.
- Tests cover slashes, whitespace, punctuation, and long identifiers.

## Validation Ideas

- Add CLI tests for task ID parsing.
- Add workspace tests asserting the actual branch and path names created from task IDs.
