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
