# Release Pipeline & Dev Ergonomics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tighten the pre-merge gate with cargo-deny, path-triggered VM-attestation, canonical pre-PR checklist, typed feature branches, and consolidated AI-assistant guidance — without expanding CI infrastructure.

**Architecture:** All tracked artifacts live inside the `abox/` git repository. The parent workspace `/home/al/git/bakudo-abox/` is not a git repo; its `AGENTS.md`, `CLAUDE.md`, `.codex/instructions.md`, and `.claude/skills/` exist but are not version-controlled and are out of scope for this plan. Single source of truth for abox-specific AI guidance is `abox/AGENTS.md` (promoted from `abox/.claude/AGENTS.md`). Skills live at `abox/.claude/skills/`. Docs for humans live under `abox/docs/`. CI changes live in `abox/.github/workflows/ci.yml`.

**Tech Stack:** Rust workspace (`cargo`, `just`), GitHub Actions, Markdown docs, YAML CI, shell (install/release scripts).

**Working branch:** `docs/release-pipeline-spec` (already created; spec already committed as `980276d`). All tasks commit on this branch. Final task opens a PR.

**Spec:** `abox/docs/superpowers/specs/2026-04-14-release-pipeline-and-dev-ergonomics-design.md`

---

## File Structure

New files (created in this plan):

- `abox/AGENTS.md` — promoted canonical AI-assistant guide (git-mv from `abox/.claude/AGENTS.md`).
- `abox/.claude/AGENTS.md` — one-line stub pointing to `../AGENTS.md`.
- `abox/docs/contributing/pre-pr-checklist.md` — canonical pre-PR checklist.
- `abox/docs/contributing/branching.md` — branching conventions and PR lifecycle.
- `abox/docs/rollback.md` — rollback procedures (documents existing `ABOX_VERSION` env-var).
- `abox/.claude/skills/pre-pr-checklist.md` — skill: walk the checklist before opening a PR.
- `abox/.claude/skills/release-preparation.md` — skill: run `scripts/release.sh` with preconditions.
- `abox/.claude/skills/rootfs-awareness.md` — skill: check/rebuild rootfs after guest edits.
- `abox/.claude/skills/start-feature.md` — skill: create a typed feature branch.
- `abox/.github/pull_request_template.md` — GitHub PR template linking to the checklist.

Modified files:

- `abox/AGENTS.md` — expanded with new sections (after promotion).
- `abox/.github/workflows/ci.yml` — add cargo-deny job, VM-attestation job, doc-staleness advisory job; switch `check` job to `just ci`.

Out of scope (noted but not touched in this plan):

- `/home/al/git/bakudo-abox/AGENTS.md`, `CLAUDE.md`, `.codex/instructions.md`, `.claude/skills/*` — untracked parent-level scratch configs.
- `bakudo/` repo — equivalent adoption is a follow-up.
- `scripts/release.sh` — no changes; flow remains local.
- `scripts/install.sh` — already honors `ABOX_VERSION` at `scripts/install.sh:38-41`; no changes.

---

## Task 1: Promote `abox/.claude/AGENTS.md` to `abox/AGENTS.md`

**Why first:** All subsequent tasks reference `abox/AGENTS.md` as the canonical path. Establishing it first prevents later tasks from having to retcon.

**Files:**
- Move: `abox/.claude/AGENTS.md` → `abox/AGENTS.md`
- Create: `abox/.claude/AGENTS.md` (one-line pointer stub)

- [ ] **Step 1: Move the file with git**

Run from `/home/al/git/bakudo-abox/abox`:

```bash
git mv .claude/AGENTS.md AGENTS.md
git status
```

Expected: `renamed: .claude/AGENTS.md -> AGENTS.md`.

- [ ] **Step 2: Create pointer stub at old location**

Write `abox/.claude/AGENTS.md` with exactly this content:

```markdown
# Claude Code Agent Instructions

The canonical AI-assistant guide for this repo lives at [`../AGENTS.md`](../AGENTS.md).

This stub exists so Claude Code's `.claude/`-discovery continues to find the guide when invoked from a subdirectory. Copilot, Codex, and Claude Code all read `AGENTS.md` at the repo root; that is the authoritative source.
```

- [ ] **Step 3: Verify content is intact**

Run:
```bash
head -5 AGENTS.md
```

Expected first line: `# Claude Code Agent Instructions for `abox``.

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md .claude/AGENTS.md
git commit -m "chore(agents): promote .claude/AGENTS.md to repo-root AGENTS.md

Copilot, Codex, and Claude Code all read AGENTS.md at the repo
root. Promoting makes the single source of truth discoverable by
every tool without needing tool-specific stubs. .claude/AGENTS.md
becomes a one-line pointer so Claude Code's subdirectory discovery
still works."
```

---

## Task 2: Write `abox/docs/contributing/pre-pr-checklist.md`

**Files:**
- Create: `abox/docs/contributing/pre-pr-checklist.md`

- [ ] **Step 1: Create the directory**

```bash
mkdir -p docs/contributing
```

- [ ] **Step 2: Write the checklist file**

Write `abox/docs/contributing/pre-pr-checklist.md` with exactly this content:

```markdown
# Pre-PR Checklist

This is the canonical gate for opening a PR against `main`. Humans and AI assistants follow this exact list. CI enforces what it can; the rest is acknowledged by the PR author in the PR template.

## Always

- [ ] Working on a typed feature branch, not `main`. See [branching.md](./branching.md).
- [ ] `just check` passes (fmt + clippy + test).
- [ ] `just deny` passes (supply-chain audit).
- [ ] `./scripts/e2e_test.sh` passes phases 1–5 locally.
- [ ] Commit messages follow Conventional Commits (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`).
- [ ] Each commit subject line reads as a useful release-note bullet on its own. `scripts/release.sh` auto-generates `CHANGELOG.md` from these subjects in Keep-a-Changelog format. Bad: `fix: bug`. Good: `fix: prevent shim panic when CWD is unset on detach`.
- [ ] No `unwrap()` introduced in `abox-core`.
- [ ] If you changed a `just` recipe, a CI workflow, or a release step → the matching update to `AGENTS.md` and any relevant skill in `.claude/skills/` lands in **the same PR**.

## If you touched VM / guest / proxy code

The following paths trigger the `vm-attestation` CI check:

- `guest/**`
- `scripts/build_rootfs.sh`
- `scripts/bootstrap_vm.sh`
- `crates/abox-runtime/**`
- `crates/abox-proxy/**`
- `crates/abox-shim/**`
- `templates/config.example.toml`

If your diff includes any of those:

- [ ] If `guest/init.sh` or `scripts/build_rootfs.sh` changed, run `just rebuild-rootfs`.
- [ ] `just e2e-vm` passes locally (phases 6–7; requires bootstrapped VM and `/dev/kvm`).
- [ ] Add the `vm-attested` label to the PR.
- [ ] Post a PR comment with the timestamp and machine of the local e2e-vm run, e.g. `just e2e-vm passed 2026-04-14T10:23Z on alice-dev`.

Without the label, the `vm-attestation` CI check stays red and merge is blocked.

## Documentation updates

CHANGELOG is auto-generated by `scripts/release.sh`. You do not edit it manually. For everything else, if you changed it you update its docs in the same PR:

- [ ] `README.md` — if install steps, CLI surface, supported platforms, top-level architecture, or the benchmark table changed.
- [ ] `docs/explainer.md` — if the conceptual model, sandbox lifecycle, or policy engine behavior changed.
- [ ] `docs/decisions/` — if an architectural decision changes or is superseded, add a new ADR or amend with a dated note. Do not silently rewrite history.
- [ ] `docs/future-work.md` — if you closed a roadmap item, move it to the "Closed" section with the date and PR link.
- [ ] `templates/config.example.toml` — if any config schema changed.
- [ ] `AGENTS.md` + relevant skill in `.claude/skills/` — if `just`, CI, or release changed (already in the "Always" list above).

A non-blocking CI reminder will comment on your PR listing touched code paths and docs that *may* need updating. It is advisory; you confirm the omission was deliberate.

## After the checklist

Push the branch and open a PR. The PR template re-asserts the attestation checkboxes so you acknowledge them at PR-open time.
```

- [ ] **Step 3: Commit**

```bash
git add docs/contributing/pre-pr-checklist.md
git commit -m "docs(contributing): add canonical pre-PR checklist"
```

---

## Task 3: Write `abox/docs/contributing/branching.md`

**Files:**
- Create: `abox/docs/contributing/branching.md`

- [ ] **Step 1: Write the file**

Write `abox/docs/contributing/branching.md` with exactly this content:

```markdown
# Branching & PRs

## Branch naming convention

Every change reaches `main` through a typed feature branch. Branch names mirror the Conventional Commits prefixes so the intent is obvious from the branch alone:

| Prefix | Purpose |
|---|---|
| `feat/<slug>` | New features |
| `fix/<slug>` | Bug fixes |
| `refactor/<slug>` | Non-behavioral restructuring |
| `docs/<slug>` | Documentation only |
| `chore/<slug>` | Tooling, dependencies, CI |
| `test/<slug>` | Test-only changes |

Rules for slugs:

- kebab-case
- ≤ 40 characters
- Descriptive. `feat/cargo-deny-ci` is good; `feat/stuff` is not.

## PR lifecycle

1. Start from an up-to-date `main`:
   ```bash
   git checkout main && git pull
   git checkout -b feat/my-thing
   ```
2. Do the work. Commit early and often with Conventional-Commits subjects.
3. Before opening the PR, walk [`pre-pr-checklist.md`](./pre-pr-checklist.md).
4. `git push -u origin feat/my-thing`.
5. Open the PR. The PR template will ask you to confirm the checklist and, if applicable, add the `vm-attested` label.
6. Address review feedback by adding new commits (not by force-pushing a rewritten history) until the PR is approved.
7. Merge via **squash-merge**. The squash commit's subject is the final Conventional-Commits message that lands in `main`'s log and in the auto-generated `CHANGELOG.md`.
8. Delete the remote branch after merge.

## Branch protection on `main`

Configure in GitHub repo settings:

- Disallow direct pushes to `main` (include administrators where practical).
- Require pull request with at least one approval.
- Require status checks to pass before merging:
  - `check` (fmt + clippy + test + deny, via `just ci`)
  - `e2e-phases-1-5`
  - `vm-attestation`
  - `doc-staleness-reminder` is advisory, **not** a required check.
- Require linear history. Enforce squash-merge only.
- Require branches to be up to date before merging.

These settings live in repo config, not in code. They are documented here so the state of the repo can be verified by reading one file.

## Working with git worktrees

For risky or long-lived feature work — especially anything touching the VM boot path, the proxy, or the policy engine — prefer `git worktree add` so the work is isolated from other local changes:

```bash
git worktree add ../abox-feat-my-thing -b feat/my-thing
```

The `superpowers:using-git-worktrees` skill automates this setup.

## Do not

- Commit directly to `main`. Protection enforces this at the server; the rule here documents intent.
- Force-push a branch that has an open PR; reviewers lose context. Add fixup commits instead.
- Mix unrelated changes on one branch. One branch, one intent, one squash commit.
```

- [ ] **Step 2: Commit**

```bash
git add docs/contributing/branching.md
git commit -m "docs(contributing): add branching convention and PR lifecycle"
```

---

## Task 4: Write `abox/docs/rollback.md`

**Files:**
- Create: `abox/docs/rollback.md`

- [ ] **Step 1: Write the file**

Write `abox/docs/rollback.md` with exactly this content:

```markdown
# Rollback

Releases of `abox` are immutable GitHub releases. Rolling back a bad version means (a) installing a known-good previous version on affected hosts and (b) communicating to users that a version should be skipped. There is no automated rollback mechanism and none is planned at this scale.

## Pin an install to a specific version

`scripts/install.sh` honors the `ABOX_VERSION` environment variable. Users who already hit a bad version can re-run the installer pinned to the previous tag:

```bash
curl -fsSL https://raw.githubusercontent.com/OWNER/REPO/main/scripts/install.sh \
  | ABOX_VERSION=v0.4.2 bash
```

Or from a local checkout:

```bash
ABOX_VERSION=v0.4.2 ./scripts/install.sh
```

The installer verifies SHA256SUMS for the pinned release in the same way as for `latest`.

## Mark a release as pre-release / yank

If a published release is discovered to be broken:

1. Open the release on GitHub: `Releases` → the bad version → `Edit release`.
2. Tick `Set as a pre-release` and save.
   - The "Latest" badge moves to the previous good release.
   - `install.sh` (without `ABOX_VERSION`) now resolves `latest` to the previous good release automatically because the GitHub API excludes pre-releases from `latest`.
3. Amend the release notes at the top with a bold warning:
   ```markdown
   **⚠️ YANKED 2026-04-14 — do not install.** Regression in <area>; use v0.4.2 or v0.4.4+.
   ```
4. Publish an announcement (see template below).

Do not delete the release or the tag. The bad artifacts remain reachable for anyone who needs to reproduce a failure; the pre-release flag is sufficient to stop accidental new installs.

## Communication template

Post in the project's primary channel (Discussions, Slack, or announcement issue):

> **Known-bad release: abox vX.Y.Z**
>
> We have yanked vX.Y.Z. Symptom: <one-sentence description>. If you installed vX.Y.Z, roll back with:
>
> ```bash
> ABOX_VERSION=v<previous> ./scripts/install.sh
> ```
>
> Fix tracked in #<issue>. A patched release is targeted for <date>.

## What is not here

- No automated "yank registry" that `abox --version` checks. Users find out through GitHub or the announcement.
- No CI-driven post-release smoke test. Releases are cut locally via `scripts/release.sh`, which runs the full quality gate and e2e before tagging. That is the last-line defense against shipping broken artifacts.
```

- [ ] **Step 2: Commit**

```bash
git add docs/rollback.md
git commit -m "docs: add rollback procedure for shipped releases"
```

---

## Task 5: Expand `abox/AGENTS.md` with new sections

**Files:**
- Modify: `abox/AGENTS.md` (add three new sections; consolidate no-unwrap rule)

- [ ] **Step 1: Append new sections after the existing "Commit Messages" section**

Insert these four sections immediately after the "Commit Messages" section (after line 50 of the original file) and before the "Key Patterns" section. Use the `Edit` tool with the `new_string` below. Match the surrounding style (H3 headings).

`old_string` (three lines from the existing file):

```
- `chore:` for tooling and CI changes

### Key Patterns
```

`new_string`:

```
- `chore:` for tooling and CI changes

### Branching & PRs

Never commit directly to `main`. Every change reaches `main` through a typed feature branch and a reviewed pull request. See [`docs/contributing/branching.md`](docs/contributing/branching.md) for the naming convention (`feat/<slug>`, `fix/<slug>`, etc.), the PR lifecycle, and the branch-protection settings.

When starting new work, the `start-feature` skill in `.claude/skills/` creates the branch and enforces naming.

### Before Opening a PR

Walk the canonical [`docs/contributing/pre-pr-checklist.md`](docs/contributing/pre-pr-checklist.md). It is the single source of truth that CI, the PR template, and the `pre-pr-checklist` skill all reference. Do not duplicate its contents here — read it directly.

Key gates at a glance:

- `just check` and `just deny` pass.
- `scripts/e2e_test.sh` phases 1–5 pass locally.
- If the diff touches VM/guest/proxy code (see the checklist for the exact path list), `just e2e-vm` passes and the PR carries the `vm-attested` label.

### When You Change `just`, CI, or Release Steps

Tooling changes ship with their documentation update **in the same PR**. If you:

- Add or modify a recipe in `justfile` →
- Add or modify a workflow under `.github/workflows/` →
- Change a step in `scripts/release.sh` →

…then the same PR must update `AGENTS.md` and any affected skill in `.claude/skills/`. The pre-PR checklist and an advisory CI reminder both flag this; the PR is not complete until the docs reflect the new reality. This keeps AI assistants (Copilot, Codex, Claude Code) from steering future contributors toward stale commands.

### Documentation Updates

CHANGELOG is auto-generated by `scripts/release.sh` from Conventional-Commits messages. Do not hand-edit it. For everything else, the rule is "if you changed behavior, update the doc that describes it." The full list (README, explainer, ADRs, future-work, config example) lives in [`docs/contributing/pre-pr-checklist.md`](docs/contributing/pre-pr-checklist.md#documentation-updates).

### Key Patterns
```

Use the `Edit` tool to perform this replacement.

- [ ] **Step 2: Verify the no-unwrap rule is still present in the "Quality Standards" section**

Run:
```bash
grep -n "unwrap" AGENTS.md
```

Expected: at least one match on the line containing "No `unwrap()` in library code". This is the canonical location; it should remain as-is (no change).

- [ ] **Step 3: Commit**

```bash
git add AGENTS.md
git commit -m "docs(agents): add branching, pre-PR, and doc-update rules"
```

---

## Task 6: Create `abox/.claude/skills/pre-pr-checklist.md` skill

**Files:**
- Create: `abox/.claude/skills/pre-pr-checklist.md`

- [ ] **Step 1: Create the skills directory**

```bash
mkdir -p .claude/skills
```

- [ ] **Step 2: Write the skill**

Write `abox/.claude/skills/pre-pr-checklist.md` with exactly this content:

```markdown
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
- `crates/abox-runtime/**`
- `crates/abox-proxy/**`
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
```

- [ ] **Step 3: Commit**

```bash
git add .claude/skills/pre-pr-checklist.md
git commit -m "chore(skills): add pre-pr-checklist skill"
```

---

## Task 7: Create `abox/.claude/skills/release-preparation.md` skill

**Files:**
- Create: `abox/.claude/skills/release-preparation.md`

- [ ] **Step 1: Write the skill**

Write `abox/.claude/skills/release-preparation.md` with exactly this content:

```markdown
---
name: release-preparation
description: Use when the user asks to cut a release, run release.sh, or tag a new version (e.g. "release v0.5.0"). Walks scripts/release.sh preconditions, invokes it, and explains the rollback path if the release turns out bad.
---

# Release Preparation

Cutting a release runs `scripts/release.sh <version>`. That script is the 12-step source of truth; this skill makes sure the preconditions are met and the outputs are understood.

## When to invoke

- User says "cut a release v<X.Y.Z>" / "release the next version" / "time to tag".
- User invokes `just release <version>`.

## Preconditions (check before running)

- `git status` shows a clean working tree on `main` (up to date with `origin/main`).
- `~/.abox/vm/cloud-hypervisor` and `~/.abox/vm/rootfs.raw` exist (bootstrapped VM).
- `/dev/kvm` exists and is accessible to the current user (the benchmark step requires it).
- The version number follows SemVer: `v<major>.<minor>.<patch>`. The leading `v` is optional; `release.sh` normalizes.
- The Always gates from the pre-PR checklist are green on `main`: `just check`, `just deny`, `./scripts/e2e_test.sh`.

If any precondition fails, stop and report. Do not pass `--force` flags to bypass.

## Invocation

```bash
just release <version>
# equivalent to:
./scripts/release.sh <version>
```

Use `just release-dry <version>` first to see the plan without committing or tagging.

## What the script does (summary; see `scripts/release.sh:47-55` for the definitive list)

1. Preflight (clean tree, version validity, bootstrap present).
2. Bump `Cargo.toml` + `Cargo.lock`.
3. Run fmt / clippy / test.
4. Run `scripts/e2e_test.sh` (all phases, including 6–7).
5. Build `--release`.
6. Run VM benchmarks (5 runs, average, write to `benchmarks/<version>.json`).
7. Update the benchmark table in `README.md`.
8. Regenerate `README.md` top-level sections (if changed).
9. Generate `CHANGELOG.md` entry from `git log` since last tag (Keep-a-Changelog format).
10. `cargo install --path` locally so the developer has the new binary.
11. `git commit` version bump + benchmarks + changelog.
12. `git tag v<version>` (no push).

After step 12, the developer pushes manually: `git push origin main --tags`. The tag push triggers the `release.yml` GitHub Actions workflow which builds binaries + VM assets and publishes a GitHub release.

## After tag push

- Watch the `release.yml` workflow. If it fails, the tag still exists but the release is not published. Fix and push a new tag, or delete the tag and retry.
- Install the published release on a clean machine via `./scripts/install.sh` and run `abox --version` to sanity-check.
- Announce.

## If the release turns out bad

Follow [`docs/rollback.md`](../../docs/rollback.md):

1. Mark the release as pre-release on GitHub (not deleted).
2. Amend release notes with a bold YANKED warning.
3. Post the communication template.
4. Cut a patch release with the fix.

Do not delete tags; do not rewrite history. Immutable artifacts are the rollback mechanism.

## Do not

- Edit `CHANGELOG.md` by hand as part of release prep. The script generates it from commit messages.
- Skip the e2e step. Phase 6–7 regressions only surface here.
- Push the tag before `release.sh` has committed the version bump + benchmarks.
```

- [ ] **Step 2: Commit**

```bash
git add .claude/skills/release-preparation.md
git commit -m "chore(skills): add release-preparation skill"
```

---

## Task 8: Create `abox/.claude/skills/rootfs-awareness.md` skill

**Files:**
- Create: `abox/.claude/skills/rootfs-awareness.md`

- [ ] **Step 1: Write the skill**

Write `abox/.claude/skills/rootfs-awareness.md` with exactly this content:

```markdown
---
name: rootfs-awareness
description: Use after editing guest/init.sh, scripts/build_rootfs.sh, or any file that feeds into the guest rootfs image. Ensures `just check-rootfs` is run to detect staleness, and `just rebuild-rootfs` is run when the rootfs is out of date.
---

# Rootfs Awareness

The guest rootfs (`~/.abox/vm/rootfs.raw`) is built from `guest/init.sh`, `scripts/build_rootfs.sh`, and the embedded shim binary. It is *not* regenerated automatically. If you edit any of the inputs and forget to rebuild, your local tests and e2e runs exercise a stale guest — which can hide regressions or surface phantom ones.

## When to invoke

Triggered by edits to any of:

- `guest/**` (especially `guest/init.sh`)
- `scripts/build_rootfs.sh`
- `scripts/bootstrap_vm.sh`
- `crates/abox-shim/**` (the shim is embedded into the rootfs)

## Process

### 1. Detect staleness

```bash
just check-rootfs
```

`check-rootfs` compares the current input hashes (stored in `rootfs.raw.inputs`) against the hash of the built image. If they differ, the recipe prints a warning — it does **not** auto-rebuild.

### 2. Rebuild if stale

If `check-rootfs` warns:

```bash
just rebuild-rootfs
```

This runs `scripts/build_rootfs.sh` which uses `fakeroot` to assemble the ext4 image without sudo. The rebuild takes 1–3 minutes depending on network for Alpine packages.

Verify success with a quick smoke run:

```bash
abox run --task rootfs-smoke --ephemeral -- \
  bash -c "echo ok && which claude && which codex && node --version"
```

Expected output includes `ok`, paths to both CLIs, and a Node version.

### 3. Reflect in the PR

The rebuilt rootfs is a host artifact, not committed. But the changes that caused the rebuild live in the diff. Any edit to `guest/**` or `scripts/build_rootfs.sh` triggers the `vm-attestation` path filter (see [`pre-pr-checklist.md`](../../docs/contributing/pre-pr-checklist.md)), so `just e2e-vm` must pass and the `vm-attested` label must be on the PR.

## Common failure modes

- `bash: command not found`, `sync: command not found`: Alpine package staging bug. Re-run `just rebuild-rootfs`; inspect `scripts/build_rootfs.sh` apk extraction.
- `claude: command not found` inside the guest: npm install in build_rootfs.sh failed silently. Check network; re-run with `-x`.
- `fakeroot: command not found` on host: install `fakeroot` (`apt install fakeroot` / `dnf install fakeroot`).
- `rootfs.raw not found`: bootstrap has not run. `just bootstrap-vm` first, then `just rebuild-rootfs`.

## Related

- `rebuild-and-smoke.md` — longer-form rebuild-plus-smoke workflow.
- `integration-test.md` — cross-repo end-to-end test (bakudo → abox).
```

- [ ] **Step 2: Commit**

```bash
git add .claude/skills/rootfs-awareness.md
git commit -m "chore(skills): add rootfs-awareness skill"
```

---

## Task 9: Create `abox/.claude/skills/start-feature.md` skill

**Files:**
- Create: `abox/.claude/skills/start-feature.md`

- [ ] **Step 1: Write the skill**

Write `abox/.claude/skills/start-feature.md` with exactly this content:

```markdown
---
name: start-feature
description: Use when starting new work in abox — "let's build X", "fix bug Y", "start on Z". Creates a typed feature branch from main following the naming convention in docs/contributing/branching.md, and recommends a worktree for high-risk surfaces.
---

# Start Feature

Every change reaches `main` through a typed feature branch. Never commit on `main`. This skill ensures the branch exists with the right name before any work begins.

## When to invoke

- User says "let's start on <X>" / "begin a new feature" / "fix bug <Y>" / "add <Z>".
- Beginning implementation of a plan's first task.

## Process

### 1. Sync `main`

```bash
git fetch origin
git status
```

If not on `main`, `git checkout main`. If behind `origin/main`, `git pull --ff-only`. Abort if the working tree has uncommitted changes — stash or commit them first.

### 2. Choose a prefix

Match the intent to a Conventional-Commits prefix:

| Intent | Prefix |
|---|---|
| New feature | `feat/` |
| Bug fix | `fix/` |
| Refactor (no behavior change) | `refactor/` |
| Docs only | `docs/` |
| Tooling / deps / CI | `chore/` |
| Tests only | `test/` |

### 3. Choose a slug

- kebab-case
- ≤ 40 characters
- Descriptive — name the *thing*, not the activity. `feat/cargo-deny-ci` not `feat/add-thing`.

Propose the full branch name to the user before creating it. Confirm.

### 4. Create the branch

```bash
git checkout -b <prefix>/<slug>
```

### 5. Worktree recommendation (for high-risk work)

If the work will touch any of the VM-attestation paths (see [`docs/contributing/pre-pr-checklist.md`](../../docs/contributing/pre-pr-checklist.md)) or involves long-lived experimentation, suggest the `superpowers:using-git-worktrees` skill before diving in. Isolated worktrees prevent cross-contamination with other in-flight changes on the same machine.

### 6. Hand off

Work begins. Remember: the terminal state of this branch is the `pre-pr-checklist` skill, which walks the gate before the PR is opened.

## Do not

- Skip step 1. Branching from a stale or dirty `main` is the most common cause of messy PRs.
- Invent a prefix outside the table. CI and the CHANGELOG generator rely on these exact strings.
- Use the slug `fix`, `feat`, `wip`, or anything that doesn't name the actual subject.
```

- [ ] **Step 2: Commit**

```bash
git add .claude/skills/start-feature.md
git commit -m "chore(skills): add start-feature skill"
```

---

## Task 10: Create `abox/.github/pull_request_template.md`

**Files:**
- Create: `abox/.github/pull_request_template.md`

- [ ] **Step 1: Write the PR template**

Write `abox/.github/pull_request_template.md` with exactly this content:

```markdown
## Summary

<!-- 1-3 sentences. What changed and why. -->

## Pre-PR checklist

Follow [`docs/contributing/pre-pr-checklist.md`](../docs/contributing/pre-pr-checklist.md). Confirm:

- [ ] `just check` passes locally.
- [ ] `just deny` passes locally.
- [ ] `./scripts/e2e_test.sh` phases 1–5 pass locally.
- [ ] On a typed feature branch, not `main`.
- [ ] Conventional-Commits subject lines (they feed the auto-generated `CHANGELOG.md`).
- [ ] No `unwrap()` added in `abox-core`.

## VM / guest / proxy changes

If this PR touches any of `guest/**`, `scripts/build_rootfs.sh`, `scripts/bootstrap_vm.sh`, `crates/abox-runtime/**`, `crates/abox-proxy/**`, `crates/abox-shim/**`, or `templates/config.example.toml`:

- [ ] `just e2e-vm` passed locally (phases 6–7).
- [ ] I have added the `vm-attested` label.
- [ ] I have posted a comment below with the run timestamp and machine, e.g. `just e2e-vm passed 2026-04-14T10:23Z on alice-dev`.

If this PR does **not** touch those paths, check this instead:

- [ ] This PR does not touch VM / guest / proxy paths.

## Documentation updates

<!-- Tick all that apply. See docs/contributing/pre-pr-checklist.md#documentation-updates. -->

- [ ] `README.md`
- [ ] `docs/explainer.md`
- [ ] `docs/decisions/` (ADR added or amended)
- [ ] `docs/future-work.md` (closed an item)
- [ ] `templates/config.example.toml`
- [ ] `AGENTS.md` + affected skill under `.claude/skills/` (required if you changed `justfile`, `.github/workflows/**`, or `scripts/release.sh`)
- [ ] No doc updates needed for this PR.

## Notes for reviewer

<!-- Anything non-obvious: migration steps, behavior to watch, edge cases tested. -->
```

- [ ] **Step 2: Commit**

```bash
git add .github/pull_request_template.md
git commit -m "chore(github): add PR template with checklist acknowledgment"
```

---

## Task 11: Switch the `check` CI job to `just ci`

**Why this task before adding new jobs:** We want the `cargo fmt/clippy/test` steps consolidated before we layer in cargo-deny as a separate job. Using `just ci` ensures local and CI run the same command string, but `just ci` already includes `deny`. We'll split: the `check` job uses `just check` (fmt+clippy+test only), and `cargo-deny` becomes a parallel job added in Task 12. This keeps each job's failures attributable.

**Files:**
- Modify: `abox/.github/workflows/ci.yml`

- [ ] **Step 1: Replace the three script steps with a single `just check`**

Use the `Edit` tool.

`old_string`:

```
      - name: cargo fmt --check
        run: cargo fmt --all -- --check

      - name: cargo clippy (workspace, all targets, deny warnings)
        run: cargo clippy --workspace --all-targets -- -D warnings

      - name: cargo test (workspace)
        run: cargo test --workspace
```

`new_string`:

```
      - name: Install just
        uses: extractions/setup-just@v2

      - name: just check (fmt + clippy + test)
        run: just check
```

- [ ] **Step 2: Validate the YAML parses**

```bash
python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/ci.yml'))"
```

Expected: exits 0 with no output.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run just check instead of inlined cargo invocations

Keeps the local and CI commands identical so they can't drift.
The cargo-deny step is added as a separate job in the next commit."
```

---

## Task 12: Add `cargo-deny` CI job

**Files:**
- Modify: `abox/.github/workflows/ci.yml`

- [ ] **Step 1: Append the `cargo-deny` job**

Use the `Edit` tool to insert a new job after the `check` job and before the `e2e-phases-1-5` job.

`old_string` (end of `check` job; start of `e2e-phases-1-5`):

```
      - name: just check (fmt + clippy + test)
        run: just check

  e2e-phases-1-5:
```

`new_string`:

```
      - name: just check (fmt + clippy + test)
        run: just check

  cargo-deny:
    name: supply-chain audit (cargo-deny)
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable

      - name: Cache cargo registry
        uses: Swatinem/rust-cache@v2

      - name: Install cargo-deny
        run: cargo install --locked cargo-deny

      - name: cargo deny check
        run: cargo deny check

  e2e-phases-1-5:
```

- [ ] **Step 2: Validate YAML**

```bash
python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/ci.yml'))"
```

Expected: exits 0.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add cargo-deny job for supply-chain audit

Runs \`cargo deny check\` against the workspace on every PR.
Mirrors the local \`just deny\` recipe. Required status check per
docs/contributing/branching.md."
```

---

## Task 13: Add VM-attestation path-filter CI job

**Files:**
- Modify: `abox/.github/workflows/ci.yml`

- [ ] **Step 1: Append the `vm-attestation` job**

Use the `Edit` tool to insert a new job at the end of the file (after `e2e-phases-1-5`).

`old_string` (last lines of current file):

```
      - name: Run e2e script
        # Phase 6 is gated on ~/.abox/vm/cloud-hypervisor + rootfs.raw
        # existing, which they don't on a stock GitHub runner. The script
        # prints "skipped: VM artifacts not found" and continues.
        run: ./scripts/e2e_test.sh
```

`new_string`:

```
      - name: Run e2e script
        # Phase 6 is gated on ~/.abox/vm/cloud-hypervisor + rootfs.raw
        # existing, which they don't on a stock GitHub runner. The script
        # prints "skipped: VM artifacts not found" and continues.
        run: ./scripts/e2e_test.sh

  vm-attestation:
    name: VM attestation (label required when VM paths touched)
    runs-on: ubuntu-latest
    if: github.event_name == 'pull_request'
    steps:
      - name: Checkout
        uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Detect VM-touching paths
        id: filter
        uses: dorny/paths-filter@v3
        with:
          filters: |
            vm:
              - 'guest/**'
              - 'scripts/build_rootfs.sh'
              - 'scripts/bootstrap_vm.sh'
              - 'crates/abox-runtime/**'
              - 'crates/abox-proxy/**'
              - 'crates/abox-shim/**'
              - 'templates/config.example.toml'

      - name: Enforce vm-attested label when VM paths touched
        if: steps.filter.outputs.vm == 'true'
        uses: actions/github-script@v7
        with:
          script: |
            const labels = context.payload.pull_request.labels.map(l => l.name);
            if (!labels.includes('vm-attested')) {
              core.setFailed(
                'This PR touches VM/guest/proxy code. Run `just e2e-vm` locally, ' +
                'then add the `vm-attested` label and comment with the timestamp. ' +
                'See docs/contributing/pre-pr-checklist.md.'
              );
            } else {
              core.info('vm-attested label present. Trust-but-post-a-timestamp-comment.');
            }

      - name: No-op when VM paths untouched
        if: steps.filter.outputs.vm != 'true'
        run: echo "No VM-touching paths in this PR; attestation not required."
```

- [ ] **Step 2: Validate YAML**

```bash
python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/ci.yml'))"
```

Expected: exits 0.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add vm-attestation job (requires label for VM-path PRs)

Uses dorny/paths-filter to detect PRs touching guest/, proxy, shim,
runtime, or the rootfs/bootstrap scripts. When matched, the job fails
unless the PR carries the \`vm-attested\` label — the author adds it
after running \`just e2e-vm\` locally (CI cannot run it; no /dev/kvm
on stock GitHub runners). See docs/contributing/pre-pr-checklist.md."
```

---

## Task 14: Add doc-staleness advisory CI job

**Files:**
- Modify: `abox/.github/workflows/ci.yml`

- [ ] **Step 1: Append the `doc-staleness-reminder` job**

Use the `Edit` tool to append to the end of the file.

`old_string` (end of Task-13 addition):

```
      - name: No-op when VM paths untouched
        if: steps.filter.outputs.vm != 'true'
        run: echo "No VM-touching paths in this PR; attestation not required."
```

`new_string`:

```
      - name: No-op when VM paths untouched
        if: steps.filter.outputs.vm != 'true'
        run: echo "No VM-touching paths in this PR; attestation not required."

  doc-staleness-reminder:
    name: doc staleness reminder (advisory)
    runs-on: ubuntu-latest
    if: github.event_name == 'pull_request'
    permissions:
      pull-requests: write
    steps:
      - name: Checkout
        uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Detect code-vs-docs drift
        id: filter
        uses: dorny/paths-filter@v3
        with:
          filters: |
            code:
              - 'crates/**'
              - 'scripts/**'
              - 'templates/**'
              - 'guest/**'
              - 'justfile'
              - '.github/workflows/**'
            docs:
              - 'README.md'
              - 'docs/**'
              - 'AGENTS.md'
              - '.claude/skills/**'

      - name: Post sticky comment if drift detected
        if: steps.filter.outputs.code == 'true' && steps.filter.outputs.docs != 'true'
        uses: marocchino/sticky-pull-request-comment@v2
        with:
          header: doc-staleness-reminder
          message: |
            ### Documentation reminder (advisory)

            This PR changes code but does not update any docs. That is often fine, but the pre-PR checklist asks you to confirm it was deliberate.

            Touched code paths may map to:
            - `README.md` — install, CLI surface, platforms, benchmark table
            - `docs/explainer.md` — conceptual model
            - `docs/decisions/` — ADRs
            - `docs/future-work.md` — closed roadmap items
            - `AGENTS.md` + `.claude/skills/**` — **required** if you changed `justfile`, `.github/workflows/**`, or `scripts/release.sh`

            See [`docs/contributing/pre-pr-checklist.md`](../blob/main/docs/contributing/pre-pr-checklist.md#documentation-updates). This reminder is advisory and does not block merge.

      - name: Clear sticky comment if drift resolved
        if: steps.filter.outputs.code != 'true' || steps.filter.outputs.docs == 'true'
        uses: marocchino/sticky-pull-request-comment@v2
        with:
          header: doc-staleness-reminder
          delete: true
```

- [ ] **Step 2: Validate YAML**

```bash
python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/ci.yml'))"
```

Expected: exits 0.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add advisory doc-staleness reminder job

Posts a sticky PR comment when code changes without matching doc
changes. Not a required check — the pre-PR checklist is where the
author confirms the omission was deliberate. Self-clears if docs
are added later."
```

---

## Task 15: Final verification and PR

**Files:** none modified; verification only.

- [ ] **Step 1: Run the full local quality gate**

```bash
just check
just deny
./scripts/e2e_test.sh
```

Expected: all three commands exit 0.

- [ ] **Step 2: Verify all new files are committed**

```bash
git status
git log --oneline main..HEAD
```

Expected: `git status` reports a clean working tree. `git log` shows ~11 commits on this branch (one per task, including the spec commit `980276d`).

- [ ] **Step 3: Verify the YAML one last time**

```bash
python3 -c "import yaml; y = yaml.safe_load(open('.github/workflows/ci.yml')); print('jobs:', list(y['jobs'].keys()))"
```

Expected: `jobs: ['check', 'cargo-deny', 'e2e-phases-1-5', 'vm-attestation', 'doc-staleness-reminder']`.

- [ ] **Step 4: Push the branch and open the PR**

```bash
git push -u origin docs/release-pipeline-spec
gh pr create --title "docs/ci: release pipeline and dev ergonomics" --body "$(cat <<'EOF'
## Summary

Ships the pre-merge gate described in `docs/superpowers/specs/2026-04-14-release-pipeline-and-dev-ergonomics-design.md`: cargo-deny in CI, path-triggered VM-attestation, canonical pre-PR checklist, typed feature branch convention, promoted `AGENTS.md`, and four new Claude Code skills (`pre-pr-checklist`, `release-preparation`, `rootfs-awareness`, `start-feature`).

## What changed

- **CI** — added `cargo-deny`, `vm-attestation`, and `doc-staleness-reminder` jobs; `check` job now runs `just check`.
- **Docs** — new `docs/contributing/pre-pr-checklist.md`, `docs/contributing/branching.md`, `docs/rollback.md`.
- **AI guidance** — `.claude/AGENTS.md` promoted to repo-root `AGENTS.md` (Copilot/Codex/Claude Code single source); expanded with Branching, Pre-PR, and tooling-doc-sync sections.
- **Skills** — four new skills under `.claude/skills/`.
- **PR template** — `.github/pull_request_template.md` with checklist acknowledgments.

Refer to the spec commit (`980276d`) for the full design rationale.

## Pre-PR checklist

- [x] `just check` passes
- [x] `just deny` passes
- [x] `./scripts/e2e_test.sh` phases 1–5 pass
- [x] Typed feature branch (`docs/release-pipeline-spec`)
- [x] Conventional-Commits messages
- [x] No `unwrap()` added (no Rust code touched)

## VM / guest / proxy changes

- [x] This PR does not touch VM / guest / proxy paths.

## Documentation updates

- [x] `AGENTS.md` + `.claude/skills/` (new tooling ships with its guidance — this PR *is* the tooling change).
- [x] New `docs/contributing/` and `docs/rollback.md`.

## Follow-up (out of scope, but noted)

- Enable branch protection on `main` in repo settings per `docs/contributing/branching.md`. This is config, not code.
- Verify the `dorny/paths-filter@v3`, `actions/github-script@v7`, `marocchino/sticky-pull-request-comment@v2`, and `extractions/setup-just@v2` action versions are the latest majors; pin to SHA if the project prefers.
- Parent-level untracked configs (`/home/al/git/bakudo-abox/{AGENTS.md,CLAUDE.md,.codex/,.claude/}`) were intentionally not modified. If the user wants them deduped to point at `abox/AGENTS.md`, that is a follow-up manual edit (nothing to commit).
EOF
)"
```

Expected: PR URL printed.

- [ ] **Step 5: Verify CI starts**

Open the PR URL. Within ~30s, five checks should appear: `check`, `cargo-deny`, `e2e-phases-1-5`, `vm-attestation`, `doc-staleness-reminder`. The first four should all go green. The `doc-staleness-reminder` is advisory and should NOT post a comment on this PR (docs were updated).

- [ ] **Step 6: Report completion**

Report the PR URL and the CI state to the user. Remind them:

- Branch protection on `main` is a repo-settings toggle they need to flip — the plan documents the desired state in `docs/contributing/branching.md`.
- Parent-level untracked configs were not touched; they can manually dedupe them to point at `abox/AGENTS.md` if desired.

---

## Self-review notes

- **Spec coverage:** every §1–§7 item in the spec maps to a task. §2.1 (cargo-deny) → Task 12. §2.2 (VM-attestation) → Task 13. §2.3 (just ci) → Task 11. §2.4 (doc-staleness) → Task 14. §2.5 (keep e2e-phases-1-5) → not-modified, explicitly verified in Task 15 step 3. §2.6 (branch protection) → documented in Task 3. §2.7 (explicitly not added) → honored throughout. §3 (pre-PR checklist) → Tasks 2, 10. §4 (rollback) → Task 4. §5 (AI config) → Tasks 1, 5, 6, 7, 8, 9. §6 (branching) → Tasks 3, 9. §7 (deliverables) → Task 15 step 2.
- **No placeholders:** every doc and skill file is written in full, inline.
- **Type/path consistency:** all cross-references to `docs/contributing/pre-pr-checklist.md`, `AGENTS.md`, `.claude/skills/**` use the same paths throughout.
- **Install.sh ABOX_VERSION task omitted intentionally:** verified already works at `scripts/install.sh:38-41`. No task needed; `docs/rollback.md` (Task 4) documents existing behavior.
- **Parent-level cross-repo docs:** out of scope, called out in File Structure and in the final PR body.
