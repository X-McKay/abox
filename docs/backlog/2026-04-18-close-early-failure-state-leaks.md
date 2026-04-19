# Close Early-Failure State Leaks During Sandbox and Worktree Creation

**Created:** 2026-04-18  
**Source:** `abox` repository review  
**Priority:** P1  
**Effort:** M  
**Severity:** Medium  
**Area:** workspace lifecycle, cleanup, failure handling  
**Related:** [`2026-04-08-vm-e2e-mvp-followups.md`](./2026-04-08-vm-e2e-mvp-followups.md), [`2026-04-18-sanitize-task-ids-at-the-boundary.md`](./2026-04-18-sanitize-task-ids-at-the-boundary.md)

## Summary

Most of the main sandbox-start failure path already does rollback correctly, but there are still two narrow early-failure cases that can leak repo state:

- `create_sandbox()` creates the worktree before validating that a requested template exists.
- `Git2Workspace::create_worktree()` creates the branch before checking whether the destination worktree directory already exists.

In both cases, an error can leave behind branches or worktrees that the user did not intend to create.

## Why It Matters

Leaked state makes the workspace feel unreliable and creates unnecessary cleanup work. It also makes later failures harder to reason about because the repo may already contain partial artifacts from a previous attempt.

The project already tries to roll back state on VM start failures, so these gaps are inconsistent with the existing design intent.

## Current Behavior

The important nuance is that VM start failures are already handled with rollback in `create_sandbox()`. The remaining gaps happen earlier, before that rollback path can help.

Examples:

- a missing template causes `create_sandbox()` to fail after the worktree already exists;
- a duplicate task ID can fail after the branch has been created but before the worktree directory is usable.

## Affected Code

- `crates/abox-core/src/sandbox.rs`
- `crates/abox-core/src/adapters/git2_workspace.rs`

## Recommended Fix

Shift validations earlier and make rollback atomic where early mutation is unavoidable.

1. In `create_sandbox()`, validate template existence before creating the worktree.
2. In `Git2Workspace::create_worktree()`, validate the target directory and branch preconditions before creating either artifact.
3. If mutation must happen in stages, add rollback on every failing branch.
4. Add regression tests for the exact failure modes.

## Suggested Implementation Notes

- Consider a helper that computes and validates all worktree inputs before touching git state.
- Keep rollback paths best-effort but loud in logs when cleanup fails.
- Document which methods are intended to be atomic from the caller’s point of view.
- If task ID validation changes, re-check this item because invalid IDs and stale IDs can overlap operationally.

## Acceptance Criteria

- Missing templates fail without creating a worktree or branch.
- Duplicate or stale task IDs fail without leaving a new branch behind.
- Failure tests assert both filesystem state and git ref state after the error.

## Validation Ideas

- Add unit tests around `Git2Workspace::create_worktree()`.
- Add an integration test for `create_sandbox()` with a non-existent template.
