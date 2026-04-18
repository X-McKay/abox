---
name: release-preparation
description: Use when the user asks to cut a release, run release.sh, or tag a new version (e.g. "release v0.5.0"). Ensures pre-release attestation stamps exist, then walks release.sh.
---

# Release Preparation

Cutting a release runs `scripts/release.sh <version>`. That script is the 12-step source of truth; this skill makes sure the preconditions are met and the outputs are understood.

## When to invoke

- User says "cut a release v<X.Y.Z>" / "release the next version" / "time to tag".
- User invokes `just release <version>`.

## Preconditions (check before running)

- `git status` shows a clean working tree on `main` (up to date with `origin/main`).
- **`just pre-release` has been run** and all attestation stamps in `.abox-attestations/` match HEAD. If stamps are missing or stale, run `just pre-release` first.
- The version number follows SemVer: `v<major>.<minor>.<patch>`. The leading `v` is optional; `release.sh` normalizes.

If any precondition fails, stop and report. Do not use `--skip-attestation` unless the user explicitly requests it for an emergency.

## Invocation

```bash
just release <version>
# equivalent to:
./scripts/release.sh <version>
```

Use `just release-dry <version>` first to see the plan without committing or tagging.

## What the script does (summary; see `scripts/release.sh --help` for the definitive list)

1. Preflight (clean tree, version validity).
2. Verify attestation stamps (vm, bench, smoke must match HEAD and pass).
3. Bump `Cargo.toml` + `Cargo.lock`.
4. Build `--release`.
5. Update benchmark table in `README.md` (data from attestation + criterion).
6. Save benchmark JSON to `benchmarks/<version>.json`.
7. Generate `CHANGELOG.md` entry from `git log` since last tag.
8. `cargo install --path` locally so the developer has the new binary.
9. `git commit` version bump + benchmarks + changelog.
10. `git tag v<version>` (no push).

After step 10, the developer pushes manually: `git push origin main --tags`. The tag push triggers the `release.yml` GitHub Actions workflow.

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
- Skip `just pre-release`. The attestation stamps are the proof that all tiers passed.
- Use `--skip-attestation` routinely. It exists for emergencies only.
- Push the tag before `release.sh` has committed the version bump + benchmarks.
