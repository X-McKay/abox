---
name: pre-pr-checklist
description: Use before opening a PR, committing a finished change, or merging. Walks the canonical pre-PR checklist (docs/contributing/pre-pr-checklist.md), runs the required commands, and surfaces the runtime-attestation requirement when the diff touches runtime/guest/proxy code.
---

# Pre-PR Checklist

The canonical gate for shipping any change in `abox/`. Humans and AI assistants follow the same document: [`docs/contributing/pre-pr-checklist.md`](../../docs/contributing/pre-pr-checklist.md).

## When to invoke

- User says "ready to open a PR" / "ready to commit and push" / "let's merge this".
- You finished an implementation and are about to hand off.
- Final task in a plan.

## Process

### 1. Confirm branch

```bash
git branch --show-current
```

If the answer is `main` (or empty), stop and invoke the `start-feature` skill. Never commit the work directly on `main`.

### 2. Run the Always gates

```bash
just tier-ci
```

`just tier-ci` = `fmt-check + lint + test + cargo deny check`. If it fails, stop and fix. Do not report success until it is green.

### 3. Evaluate path-triggered runtime attestation

Get the list of changed paths:

```bash
git diff --name-only main...HEAD
```

If any changed path matches one of these globs, runtime attestation is required:

- `images/**`
- `crates/abox-core/**`
- `crates/abox-proxyd/**`
- `crates/abox-protocol/**`
- `crates/abox-shim/**`
- `templates/config.example.toml`

If required:

- Run `just e2e-runtime` and wait for completion. This needs virtualization (KVM or Hypervisor.framework) and the msb runtime assets under `$MSB_HOME`; it skips cleanly without them, but a skip does not count as attestation.
- Record the timestamp. When you report the result to the user, include: "Runtime attestation required. `just e2e-runtime` passed at <ISO-8601>. After opening the PR, add the `runtime-attested` label and post a PR comment with this timestamp."

### 3b. Note on release vs PR gates

The above gates are for PRs. For releases, the full pre-release validation (`just pre-release`) must pass — this includes the live runtime e2e and the agent smoke tests. See the `release-preparation` skill.

### 4. Evaluate documentation updates

For the same diff, map touched code paths to docs that may need updating (see [`docs/contributing/pre-pr-checklist.md#documentation-updates`](../../docs/contributing/pre-pr-checklist.md#documentation-updates)). Report which docs you checked and whether each needed an update in this PR.

### 5. Meta-rule: tooling-changed-so-docs-must-too

If the diff touches `justfile`, `.github/workflows/**`, `scripts/release.sh`, or `scripts/pre_release.sh`, verify that **this same PR** also updates `AGENTS.md` and any affected skill under `.claude/skills/`. If not, refuse to mark the PR ready; add the missing updates first.

If the diff changes GitHub-hosted workflow action refs, also confirm any first-party `actions/*` upgrades move to Node 24-compatible major versions so the workflow does not keep shipping known runtime deprecation warnings.

### 6. Conventional-commit message quality

Read the last N commit subject lines (`git log main..HEAD --format='%s'`). For each, ask: "If this appeared verbatim in a release-notes bullet, would a user understand what changed?" If not, propose amended messages before the PR is opened.

### 7. Report

Summarize for the user:

- All Always gates: pass / fail (with failing output).
- Runtime attestation: not required / required + timestamp / required but skipped (why).
- Doc updates made in this PR.
- Any commits with weak subject lines.

Do not mark the work complete until every required check is green.
