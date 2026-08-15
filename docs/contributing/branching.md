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
5. Open the PR. The PR template will ask you to confirm the checklist and, if applicable, add the `runtime-attested` label.
6. Address review feedback by adding new commits (not by force-pushing a rewritten history) until the PR is approved.
7. Merge via **squash-merge**. The squash commit's subject is the final Conventional-Commits message that lands in `main`'s log and in the auto-generated `CHANGELOG.md`.
8. Delete the remote branch after merge.

## Branch protection on `main`

Configure in GitHub repo settings:

- Disallow direct pushes to `main` (include administrators where practical).
- Require pull request with at least one approval.
- Require status checks to pass before merging:
  - `check` (fmt + clippy + test, via `just check`)
  - `cargo-deny`
  - `e2e-phases-1-5`
  - `runtime-attestation` (formerly `vm-attestation`)
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
