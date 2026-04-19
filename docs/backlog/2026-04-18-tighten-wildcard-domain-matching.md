# Tighten Wildcard Domain Matching for Egress Rules and TLS Bypass

**Created:** 2026-04-18  
**Source:** `abox` repository review  
**Priority:** P0  
**Effort:** S  
**Severity:** High  
**Area:** egress policy, TLS bypass, domain matching  
**Related:** [`2026-04-08-vm-e2e-mvp-followups.md`](./2026-04-08-vm-e2e-mvp-followups.md), [`../decisions/003-https-credential-injection.md`](../decisions/003-https-credential-injection.md)

## Summary

Wildcard domain matching is currently implemented with a simple `ends_with` check. That makes patterns like `*.amazonaws.com` match hostnames such as `evilamazonaws.com`, which are not real subdomains of `amazonaws.com`.

The same matching logic is used for both egress allow rules and TLS-bypass rules, so the impact is broader than a single policy feature.

## Why It Matters

The policy engine is supposed to be the narrow authorization layer for outbound network access. If wildcard rules overmatch, then:

- egress access may be granted to attacker-controlled domains that merely resemble trusted suffixes;
- TLS interception may be bypassed for domains that should not have been exempt;
- audit data becomes less trustworthy because requests may be classified under the wrong policy intent.

This weakens the core security model.

## Current Behavior

The wildcard matcher accepts a pattern when:

- the domain ends with the suffix after `*.`; and
- the domain is longer than that suffix.

It does not require a dot boundary before the suffix. As a result, `evilamazonaws.com` matches `*.amazonaws.com`.

## Affected Code

- `crates/abox-core/src/policy.rs`
- `crates/abox-core/src/egress.rs`

## Recommended Fix

Require a real subdomain boundary for wildcard matches.

1. Keep exact matching as-is.
2. For wildcard patterns, require the domain to end with `.` plus the suffix.
3. Continue rejecting the bare apex domain for wildcard-only rules unless explicitly listed.
4. Centralize the matching logic so egress policy and TLS bypass cannot drift.

## Suggested Implementation Notes

- Prefer a shared helper used by both the policy engine and the egress module.
- Keep the matching rules deliberately simple and documented.
- Avoid introducing full public-suffix parsing unless there is a demonstrated need; a strict dot-boundary rule is enough for the current bug.

## Acceptance Criteria

- `*.amazonaws.com` matches `s3.amazonaws.com`.
- `*.amazonaws.com` matches `sts.us-east-1.amazonaws.com`.
- `*.amazonaws.com` does not match `amazonaws.com`.
- `*.amazonaws.com` does not match `evilamazonaws.com`.
- The TLS bypass path and the policy-evaluation path use the same matching behavior.

## Validation Ideas

- Extend the existing unit tests with both positive and negative boundary cases.
- Add a regression test for a lookalike domain such as `evilamazonaws.com`.
