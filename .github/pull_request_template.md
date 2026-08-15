## Summary

<!-- 1-3 sentences. What changed and why. -->

## Pre-PR checklist

Follow [`docs/contributing/pre-pr-checklist.md`](../docs/contributing/pre-pr-checklist.md). Confirm:

- [ ] `just check` passes locally.
- [ ] `just deny` passes locally.
- [ ] `./scripts/local/e2e_test.sh` phases 1–5 pass locally.
- [ ] On a typed feature branch, not `main`.
- [ ] Conventional-Commits subject lines (they feed the auto-generated `CHANGELOG.md`).
- [ ] No `unwrap()` added in `abox-core`.

## Runtime / guest / proxy changes

If this PR touches any of `guest/**`, `images/**`, `scripts/build_rootfs.sh`, `scripts/bootstrap_vm.sh`, `crates/abox-core/**`, `crates/abox-proxyd/**`, `crates/abox-protocol/**`, `crates/abox-shim/**`, or `templates/config.example.toml`:

- [ ] `just e2e-runtime` passed locally (and `just e2e-vm` if the legacy Cloud Hypervisor backend is affected).
- [ ] I have added the `runtime-attested` label (`vm-attested` is accepted as a legacy alias).
- [ ] I have posted a comment below with the run timestamp and machine, e.g. `just e2e-runtime passed 2026-08-15T10:23Z on alice-dev`.

If this PR does **not** touch those paths, check this instead:

- [ ] This PR does not touch runtime / guest / proxy paths.

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
