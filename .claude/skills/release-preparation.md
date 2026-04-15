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
