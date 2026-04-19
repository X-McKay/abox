# Fix `abox attach` for File-Backed Console Output

**Created:** 2026-04-18  
**Source:** `abox` repository review  
**Priority:** P1  
**Effort:** S  
**Severity:** Medium  
**Area:** CLI behavior, console plumbing, operator UX  
**Related:** [`2026-04-08-vm-e2e-mvp-followups.md`](./2026-04-08-vm-e2e-mvp-followups.md)

## Summary

`abox attach` still assumes the VM console lives on a Unix socket and tries to connect with `socat`. The Cloud Hypervisor adapter now configures the console as a plain log file instead.

That means `attach` is out of sync with the actual VM startup path and is likely non-functional.

## Why It Matters

This is a user-facing regression in one of the primary debugging workflows. When sandbox boot or guest init fails, console access is the first place a developer looks for answers.

If `attach` is broken, users lose an advertised recovery and observability tool.

## Current Behavior

- VM startup sets `console_socket` to a path like `console-<task>.log`.
- The attach command reads that path from `VmInfo`.
- The attach command then runs `socat UNIX-CONNECT:<path>`, which only makes sense for a Unix socket, not a regular file.

## Affected Code

- `crates/abox-core/src/adapters/cloud_hypervisor.rs`
- `crates/abox-cli/src/commands/attach.rs`

## Recommended Fix

Choose one console model and align the CLI with it.

Option A:

- keep the file-backed console and redefine `attach` as a live tail of the log file.

Option B:

- restore true interactive console attachment via a supported Cloud Hypervisor console transport, if the hypervisor version in use supports one.

Given the current implementation, Option A is the lower-risk fix.

## Suggested Implementation Notes

- If `attach` becomes a tail operation, make that explicit in help text.
- Reuse the existing console tailing logic where possible instead of building a second polling loop.
- Consider a follow-up command split if interactive attach and log-tail are both desired later.

## Acceptance Criteria

- `abox attach <task>` works against the current console implementation.
- The command follows live output rather than failing with a socket error.
- Help text accurately describes the behavior.
- A test covers the current console transport expectation.

## Validation Ideas

- Add a unit or integration test for file-backed console attachment.
- Run a sandbox locally and verify that `attach` shows the guest init banner and trailing output.
