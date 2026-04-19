# Validate Environment Variable Names Before Rendering `runner.sh`

**Created:** 2026-04-18  
**Source:** `abox` repository review  
**Priority:** P1  
**Effort:** S  
**Severity:** Medium-High  
**Area:** guest bootstrap, shell generation, privilege boundary  
**Related:** [`2026-04-08-vm-e2e-mvp-followups.md`](./2026-04-08-vm-e2e-mvp-followups.md), [`../decisions/004-non-root-guest-execution.md`](../decisions/004-non-root-guest-execution.md)

## Summary

`abox run --env KEY=VALUE` accepts arbitrary keys, and `BootMeta::runner_script()` writes them into shell as raw `export <key>='value'` lines. Values are shell-escaped, but keys are not validated.

A crafted key can break shell syntax or inject additional commands before the script drops privileges from root to the `abox` user.

## Why It Matters

This code path runs inside guest init before `su-exec` switches to the unprivileged account. A malformed or malicious environment variable name therefore executes in the highest-privilege part of the guest bootstrap path.

Even though the execution happens in the guest rather than on the host, it still matters because:

- it weakens the guest hardening story;
- it makes the bootstrap path fragile and surprising;
- it turns a normal CLI input surface into a shell-injection surface.

## Current Behavior

The CLI parses `--env` entries into `(key, value)` pairs without validating the key. `runner.sh` later interpolates the key directly into `export` statements.

The code correctly escapes values but assumes keys are already safe shell identifiers.

## Affected Code

- `crates/abox-cli/src/commands/run.rs`
- `crates/abox-core/src/boot_meta.rs`

## Recommended Fix

Reject invalid environment variable names before they reach the script generator.

1. Define a strict validation rule for keys, such as POSIX shell identifier syntax: `[A-Za-z_][A-Za-z0-9_]*`.
2. Enforce that rule at CLI parsing time and return a clear user-facing error.
3. Keep a defensive check in `runner_script()` or its inputs so unsafe keys cannot slip through other call paths.
4. Document the accepted format in `abox run --help`.

## Suggested Implementation Notes

- Add a small parser helper for `KEY=VALUE`.
- Consider moving env parsing into a shared function that returns `Result<Vec<(String, String)>>`.
- Use the same validation rule everywhere the project renders shell `export` statements.

## Acceptance Criteria

- Valid keys like `FOO`, `FOO_BAR`, and `A1` are accepted.
- Invalid keys like `1FOO`, `FOO-BAR`, `FOO BAR`, and shell metacharacter payloads are rejected with a clear error.
- `runner.sh` never emits a raw `export` line with an unvalidated identifier.

## Validation Ideas

- Add CLI unit tests for `--env` parsing.
- Add `BootMeta` tests proving unsafe keys are rejected or never rendered.
- Add a regression test with a deliberately malicious key string.
