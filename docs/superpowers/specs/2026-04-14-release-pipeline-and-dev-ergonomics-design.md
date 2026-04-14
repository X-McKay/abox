# Release Pipeline & Dev Ergonomics — Design

**Date:** 2026-04-14
**Status:** Draft (awaiting user review)
**Scope:** abox repository only. New-user onboarding (install/doctor/init UX) is a separate spec.

---

## 1. Goals & Non-Goals

### Goals

1. Code merged to `main` is functionally verified appropriate to the change's risk surface.
2. Every workflow change ships with the matching AI-assistant guidance update in the same PR.
3. A documented rollback path exists for shipped releases.
4. CI enforces what it can; humans and AI follow the same checklist for the rest.
5. Feature work happens on typed branches and reaches `main` only via PR.

### Non-Goals

- Nightly scheduled CI jobs.
- Self-hosted or paid GitHub-hosted KVM runners.
- Automating `release.sh` into a CI workflow.
- Coverage metrics, performance regression gates, fuzzing.
- New-user onboarding (install UX, `abox doctor`, `abox init`) — covered by a separate spec.
- Equivalent changes in the bakudo repo — out of scope; can be a follow-up.

---

## 2. CI Changes (`abox/.github/workflows/ci.yml`)

### 2.1 Add `cargo-deny` job

A new job that runs `just deny` (advisories + license + sources). Required check on every PR.

### 2.2 Add VM-touching path-filter attestation job

A new job using [`dorny/paths-filter`](https://github.com/dorny/paths-filter) over these paths:

- `abox/guest/**`
- `abox/scripts/build_rootfs.sh`
- `abox/scripts/bootstrap_vm.sh`
- `abox/crates/abox-runtime/**`
- `abox/crates/abox-proxy/**`
- `abox/crates/abox-shim/**`
- `abox/templates/config.example.toml`

If any path matches, the job requires a PR label `vm-attested` to pass. Without the label the check stays red and merge is blocked. The PR author adds the label after running `just e2e-vm` locally and pasting a confirmation comment with the run timestamp.

### 2.3 Replace ad-hoc steps with `just ci`

The existing `check` job becomes `just ci` (already defined as `fmt-check + lint + test + deny`) so the local command and CI command are the same string. Avoids drift between developer-local and CI behavior.

### 2.4 Add doc-staleness reminder job (advisory)

A non-blocking job that uses path filters to detect PRs touching `crates/**`, `scripts/**`, `templates/**`, `guest/**`, or `justfile` *without* a corresponding change in `README.md`, `docs/**`, `abox/AGENTS.md`, or `abox/.claude/skills/**`. When detected, it posts (or updates) a sticky PR comment listing the touched paths and the docs that *might* need a matching update. Not a required check — the pre-PR checklist (§3.3) is where the author confirms the omission was deliberate.

### 2.5 Keep existing `e2e-phases-1-5` job

Already runs `scripts/e2e_test.sh` for non-VM phases. No changes.

### 2.6 Branch protection (documented, not code)

Required status checks before merge to `main`:

- `check`
- `cargo-deny`
- `e2e-phases-1-5`
- `vm-attestation`

Direct pushes to `main` disallowed (admins included where possible). Linear history via squash-merge — each PR becomes one conventional-commit message in `main`'s log.

### 2.7 Explicitly not added

- Coverage tooling.
- Nightly or scheduled jobs.
- KVM-capable runners.
- Release automation (release flow remains `scripts/release.sh` invoked locally).

---

## 3. Pre-PR Checklist (canonical artifact)

A single source of truth at `abox/docs/contributing/pre-pr-checklist.md`. Both humans and AI follow this exact document. The PR template links to it. Skill files reference it by path rather than duplicating contents — this is intentional so it stays single-source.

### 3.1 Always

- `just check` passes (fmt + clippy + test).
- `just deny` passes.
- `scripts/e2e_test.sh` passes phases 1–5 locally.
- Conventional commit message.
- No `unwrap()` introduced in `abox-core`.
- If a `just` recipe, CI workflow, or release step changed → matching update to `abox/AGENTS.md`, the relevant skill in `abox/.claude/skills/`, and `abox/.codex/instructions.md` (or its pointer) lands in the same PR.
- Working on a typed feature branch, not `main`.

### 3.2 If touching VM/guest/proxy paths (same list as the CI path filter in §2.2)

- `just rebuild-rootfs` if `guest/init.sh` or `scripts/build_rootfs.sh` changed.
- `just e2e-vm` passes (phases 6–7).
- Add the `vm-attested` label to the PR with the local run timestamp in a PR comment.

### 3.3 Documentation updates

CHANGELOG is **auto-generated** by `scripts/release.sh` from conventional-commit messages between tags (Keep-a-Changelog format). There is no manually maintained `[Unreleased]` section. The pre-PR check on changelog therefore is:

- Conventional-commit subject line reads as a useful release-note bullet on its own. (Bad: `fix: bug`. Good: `fix: prevent shim panic when CWD is unset on detach`.) The body adds context if the subject can't carry it alone.

For other documentation, the rule is "if you changed it, the docs that describe it must be updated in the same PR":

- **`README.md`** — install steps, CLI surface, supported platforms, top-level architecture diagram, benchmark table.
- **`docs/explainer.md`** — conceptual model, sandbox lifecycle, policy engine behavior.
- **`docs/decisions/`** — if an architectural decision changes or is superseded, add a new ADR or amend the existing one (do not silently rewrite history).
- **`docs/future-work.md`** — if you closed a roadmap item, move it from "Open items" to the "Closed" section with the date and link to the PR.
- **`templates/config.example.toml`** — if any config schema changed.
- **`abox/AGENTS.md` and the relevant skill** — if a `just` recipe, CI workflow, or release step changed (already covered by §3.1).

A non-blocking CI reminder (PR comment via GitHub Actions) flags PRs that touch `crates/**`, `scripts/**`, `templates/**`, `guest/**`, or `justfile` without touching `README.md`, `docs/**`, or `CHANGELOG.md`. The reminder is advisory — the PR author confirms in the checklist that the omission was deliberate.

### 3.4 PR template

`abox/.github/pull_request_template.md` (GitHub-required path; abox is its own git repo) — short, links to the checklist file, and includes the VM-attestation checkboxes inline so the author has to acknowledge them. This file is for GitHub's PR creation flow, not for AI-tool steering. Copilot/Codex/Claude Code read `AGENTS.md`, not template files.

---

## 4. Rollback (`abox/docs/rollback.md`)

A short doc covering the existing capability:

- How to pin a specific version via `ABOX_VERSION=v0.4.2 ./scripts/install.sh`. (Verify the env-var path actually works in `install.sh`; small code addition only if it does not.)
- How to mark a published GitHub release as a pre-release / yank it.
- A communication template for announcing a known-bad version.

No new code beyond a possible `install.sh` env-var verification. No automated post-release smoke. No yank advisory mechanism.

---

## 5. AI Assistant Configuration

### 5.1 Single source of truth

Promote `abox/.claude/AGENTS.md` to `abox/AGENTS.md` (repo root of the abox crate workspace). This is the convention Copilot and Codex both pick up natively. `abox/.claude/AGENTS.md` becomes a one-line stub pointing to `../AGENTS.md`, or is deleted if Claude Code's auto-discovery walks up. `abox/.codex/instructions.md` is replaced with a pointer to `AGENTS.md`. Parent `CLAUDE.md` keeps cross-repo content but stops duplicating abox-specific rules (no-`unwrap`, conventional commits, etc.) — it points to `abox/AGENTS.md` instead.

### 5.2 `abox/AGENTS.md` updates

- New "Before opening a PR" section linking to `docs/contributing/pre-pr-checklist.md` as the authoritative gate.
- New "Branching & PRs" section linking to `docs/contributing/branching.md`. Explicit rule: never commit directly to `main`; always work on a typed branch and open a PR.
- New "When you change CI, justfile recipes, or release steps" rule: matching skill / AGENTS.md update lands in the same PR.
- Consolidate the no-`unwrap` rule here only; remove duplicates elsewhere.

### 5.3 New Claude Code skills (`abox/.claude/skills/`)

- **`pre-pr-checklist.md`** — triggers on "about to open a PR", "ready to commit", "ready to merge". Walks the checklist, runs `just check` + `just deny` + e2e phases 1–5, evaluates the path-filter list in §2.2, and if any VM path is touched, runs `just e2e-vm` and instructs the assistant to remind the user to add the `vm-attested` label.
- **`release-preparation.md`** — triggers on "cut a release", "release vX.Y.Z". Walks `release.sh` preconditions (clean tree, bootstrapped VM, `/dev/kvm`), runs the script, explains the rollback path from §4.
- **`rootfs-awareness.md`** — triggers on edits to `guest/init.sh` or `scripts/build_rootfs.sh`. Tells the assistant to run `just check-rootfs` and (if stale) `just rebuild-rootfs`.
- **`start-feature.md`** — triggers on "let's start work on X", "begin a new feature", "fix bug Y". Process:
    1. Confirm on `main` and synced (`git fetch && git status`).
    2. Determine prefix from intent (feat/fix/refactor/etc.).
    3. Propose branch name; create it (`git checkout -b <prefix>/<slug>`).
    4. If the work is non-trivial or touches the VM/proxy surface, recommend `superpowers:using-git-worktrees` for isolation.
    5. Hand off to implementation.

### 5.4 Updates to existing skills

- `integration-test.md` — add a line that running e2e phases 1–5 is also part of the pre-PR checklist.
- `rebuild-and-smoke.md` — add the `just check-rootfs` step before deciding whether to rebuild; cross-link to `rootfs-awareness.md`.

### 5.5 Process rule (enforced by checklist + reinforced in CI)

Adding a new `just` recipe, CI job, or release step requires updating `abox/AGENTS.md` and any affected skill in the same PR. The pre-PR checklist (§3.1) makes this visible to humans and AI. CI path filter on `justfile`, `.github/workflows/`, and `scripts/release.sh` adds a non-blocking PR-template reminder; not a hard CI block.

---

## 6. Feature Development Workflow

### 6.1 Branch naming convention

Mirrors conventional commit prefixes for cognitive consistency:

- `feat/<slug>` — new features
- `fix/<slug>` — bug fixes
- `refactor/<slug>` — non-behavioral changes
- `docs/<slug>` — docs only
- `chore/<slug>` — tooling, deps, CI
- `test/<slug>` — test-only changes

Slugs are kebab-case, ≤40 characters, descriptive (`feat/cargo-deny-ci`, not `feat/stuff`).

### 6.2 Branch protection on `main` (documented; enabled in repo settings)

- No direct pushes (admins included where possible).
- Required status checks per §2.6.
- Linear history via squash-merge.

### 6.3 New doc: `abox/docs/contributing/branching.md`

Covers conventions above, the PR lifecycle (branch → push → PR → checklist → merge → delete branch), and links to the pre-PR checklist.

---

## 7. Deliverables Summary

New or modified files:

- `abox/.github/workflows/ci.yml` — cargo-deny job, VM-attestation job, switch to `just ci`.
- `abox/.github/pull_request_template.md` — short template linking to checklist with VM-attestation acknowledgment.
- `abox/docs/contributing/pre-pr-checklist.md` — new.
- `abox/docs/contributing/branching.md` — new.
- `abox/docs/rollback.md` — new.
- `abox/AGENTS.md` — promoted from `abox/.claude/AGENTS.md`, expanded with new sections.
- `abox/.claude/AGENTS.md` — stub pointer or removed.
- `abox/.codex/instructions.md` — replaced with pointer to `abox/AGENTS.md`.
- `abox/.claude/skills/pre-pr-checklist.md` — new.
- `abox/.claude/skills/release-preparation.md` — new.
- `abox/.claude/skills/rootfs-awareness.md` — new.
- `abox/.claude/skills/start-feature.md` — new.
- `abox/.claude/skills/integration-test.md` — minor update.
- `abox/.claude/skills/rebuild-and-smoke.md` — minor update.
- `CLAUDE.md` (parent) — deduplicate; point to `abox/AGENTS.md`.
- `scripts/install.sh` — verify `ABOX_VERSION` env-var path; small fix only if missing.

Repo-settings changes (out-of-tree, documented in `branching.md`):

- Branch protection on `main` per §2.6 / §6.2.

---

## 8. Open Questions

None blocking. The `install.sh` `ABOX_VERSION` support needs a one-line verification during implementation; if it works, the rollback doc just describes it; if not, a small patch is added then.
