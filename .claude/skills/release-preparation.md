---
name: release-preparation
description: Use when the user asks to cut a release, run release.sh, or tag a new version (e.g. "release v0.5.0"). Ensures pre-release attestation stamps exist, then walks release.sh.
---

# Release Preparation

Cutting a release runs `scripts/release.sh <version>`. That script is the 8-step source of truth; this skill makes sure the preconditions are met and the outputs are understood.

## When to invoke

- User says "cut a release v<X.Y.Z>" / "release the next version" / "time to tag".
- User invokes `just release <version>`.

## Preconditions (check before running)

- `git status` shows a clean working tree on `main` (up to date with `origin/main`).
- **`just pre-release` has been run** and the attestation stamps in `.abox-attestations/` (`runtime.json`, `smoke.json`) match HEAD. If stamps are missing or stale, run `just pre-release` first.
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
2. Verify attestation stamps against HEAD. `just pre-release` writes one per passing tier: `runtime` (MicroSandbox e2e, `just e2e-runtime`) and `smoke` (agent smoke tests).
3. Bump `Cargo.toml` + `Cargo.lock`.
4. Build `--release`.
5. Generate `CHANGELOG.md` entry from `git log` since last tag.
6. `cargo install --path` locally so the developer has the new binary.
7. `git commit` version bump + changelog.
8. `git tag v<version>` (no push).

After step 8, the developer pushes manually: `git push origin main --tags`. The tag push triggers the `release.yml` GitHub Actions workflow (binaries for Linux x86_64/aarch64 and macOS aarch64, the `abox-guest-bins-<arch>.tar.gz` guest-binary tarballs, and `SHA256SUMS`).

## After tag push

- Watch the `release.yml` workflow. If it fails, the tag still exists but the release is not published. Fix and push a new tag, or delete the tag and retry.
- Treat Node runtime deprecation annotations in `release.yml` as workflow debt. If GitHub warns that first-party `actions/*` refs still run on deprecated Node majors, update those workflow refs in a follow-up PR before the next release.
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
