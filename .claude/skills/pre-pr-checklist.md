---
name: pre-pr-checklist
description: Use before opening a PR, committing a finished change, or merging. Walks the canonical pre-PR checklist (docs/contributing/pre-pr-checklist.md), runs the required commands, and surfaces the VM-attestation requirement when the diff touches guest/proxy code.
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
just check
just deny
./scripts/e2e_test.sh
```

`just check` = `fmt-check + lint + test`. `just deny` = `cargo deny check`. The e2e script runs phases 1–5 on any host (no VM needed). If any of these fail, stop and fix. Do not report success until they are green.

### 3. Evaluate path-triggered VM attestation

Get the list of changed paths:

```bash
git diff --name-only main...HEAD
```

If any changed path matches one of these globs, VM attestation is required:

- `guest/**`
- `scripts/build_rootfs.sh`
- `scripts/bootstrap_vm.sh`
- `crates/abox-core/**`
- `crates/abox-proxyd/**`
- `crates/abox-protocol/**`
- `crates/abox-shim/**`
- `templates/config.example.toml`

If required:

- If `guest/init.sh` or `scripts/build_rootfs.sh` changed, run `just rebuild-rootfs` first.
- Run `just e2e-vm` and wait for completion. This needs a bootstrapped VM and `/dev/kvm`.
- Record the timestamp. When you report the result to the user, include: "VM attestation required. Run `just e2e-vm` passed at <ISO-8601>. After opening the PR, add the `vm-attested` label and post a PR comment with this timestamp."

### 4. Evaluate documentation updates

For the same diff, map touched code paths to docs that may need updating (see [`docs/contributing/pre-pr-checklist.md#documentation-updates`](../../docs/contributing/pre-pr-checklist.md#documentation-updates)). Report which docs you checked and whether each needed an update in this PR.

### 5. Meta-rule: tooling-changed-so-docs-must-too

If the diff touches `justfile`, `.github/workflows/**`, or `scripts/release.sh`, verify that **this same PR** also updates `AGENTS.md` and any affected skill under `.claude/skills/`. If not, refuse to mark the PR ready; add the missing updates first.

### 6. Conventional-commit message quality

Read the last N commit subject lines (`git log main..HEAD --format='%s'`). For each, ask: "If this appeared verbatim in a release-notes bullet, would a user understand what changed?" If not, propose amended messages before the PR is opened.

### 7. Report

Summarize for the user:

- All Always gates: pass / fail (with failing output).
- VM attestation: not required / required + timestamp / required but skipped (why).
- Doc updates made in this PR.
- Any commits with weak subject lines.

Do not mark the work complete until every required check is green.
