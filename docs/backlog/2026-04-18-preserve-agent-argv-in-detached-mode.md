# Preserve Agent Arguments When Re-Execing Detached Runs

**Created:** 2026-04-18  
**Source:** `abox` repository review  
**Priority:** P3  
**Effort:** S  
**Severity:** Medium  
**Area:** detached execution, argv handling, CLI correctness  
**Related:** [`2026-04-08-vm-e2e-mvp-followups.md`](./2026-04-08-vm-e2e-mvp-followups.md)

## Summary

Detached mode re-execs the current `abox` command line after stripping `--detach`, but the stripping logic removes every `--detach` token from the full argv. That includes tokens that appear after the user’s `--` separator and belong to the agent command instead of `abox` itself.

As a result, detached mode can silently change what the guest agent actually runs.

## Why It Matters

This is subtle and easy to miss because it only affects detached runs and only when the guest command also uses `--detach`. Those are exactly the kinds of cases users are unlikely to diagnose quickly.

Silent argv mutation is worse than an explicit error because it makes behavior nondeterministic between foreground and detached runs.

This still looks like a real bug, but compared with the security and isolation items above it is a lower-priority release candidate and may be better suited to post-`0.3.0` cleanup unless detached mode becomes a primary workflow.

## Current Behavior

The helper removes every string equal to `--detach` from the re-exec argv vector. It does not stop at the CLI `--` delimiter and does not distinguish between top-level `abox` flags and guest-command arguments.

## Affected Code

- `crates/abox-cli/src/commands/run.rs`

## Recommended Fix

Strip only the top-level `abox run --detach` flag.

1. Parse the argv structurally instead of filtering tokens by value.
2. Stop processing flags once the top-level `--` delimiter is reached.
3. Keep guest command arguments byte-for-byte identical between foreground and detached mode.

## Suggested Implementation Notes

- A small purpose-built argv rewriter is probably sufficient.
- Another option is to reconstruct the re-exec argv from parsed clap data instead of the raw process argv.
- If clap reconstruction is used, be careful to preserve argument ordering where it matters.

## Acceptance Criteria

- A detached run with guest command arguments containing `--detach` preserves those arguments.
- Foreground and detached mode produce the same guest command argv.
- Tests cover both top-level `--detach` removal and guest-command preservation after `--`.

## Validation Ideas

- Extend the existing `strip_detach_flag` tests with delimiter-aware cases.
- Add an integration-style test that prints the guest argv for both foreground and detached runs.
