# Changelog

## v0.3.1 — 2026-04-21

### Fixes

- fix(rootfs): fail closed guest scratch and dockerize builder (8b6520a)


## v0.3.0 — 2026-04-20

### Features

- feat(rootfs): add python3 to guest image (73088d1)
- feat(cli): add --capabilities top-level flag with config bypass (7d942f3)

### Fixes

- fix(workspace): surface non-conflict merge failures (ea51594)
- fix(proxy): confine per-vm bridge cwd to worktree (2bcbdf1)
- fix(release): finish wildcard matching and refresh v0.3.0 docs (540139f)
- fix(setup): verify virtiofsd sandbox capability (aa32be7)

### Chores

- chore: format proxy bridge cwd translation (18b9543)
- chore(cli): reformat doctor and init output helpers (acd514b)
- chore(release): bump version to 0.3.0 (fff8566)


## v0.3.0 — 2026-04-19

### Features

- feat(cli): add top-level `abox --capabilities`, a machine-readable Phase 0
  capability envelope that bypasses config / policy / runtime-dir loading so
  bakudo and other harnesses can probe abox before first-run setup
- feat(rootfs): ship Python 3 in the guest image alongside bash, Node.js/npm,
  `su-exec`, the system CA bundle, and pinned Claude Code / Codex CLIs
- feat(ux): polished `abox doctor` — ANSI color (NO_COLOR / TERM=dumb aware), section grouping,
  version header, and semantic icons matching the OpenCode/Codex CLI quality bar
- feat(ux): polished `abox init` — colored step indicators, dimmed action lines, and
  a clean summary block with version header

### Fixes

- fix(setup): `abox init` / `abox doctor` now verify that the installed
  `virtiofsd` supports namespace sandboxing and surface the exact
  `setcap 'cap_sys_admin+ep' ...` remediation when it does not
- fix(security): wildcard domain matching now requires a dot boundary — `*.amazonaws.com`
  no longer matches `evilamazonaws.com` for either egress-policy evaluation
  or TLS-bypass passthrough (regression tests added)

### Documentation

- docs(backlog): add seven tracked backlog items (P0–P2) for post-0.3.0 hardening
- docs: refresh README, tutorial, and VM setup guide for the current 768 MiB
  guest image, shipped toolchain, MITM credential-injection flow, release
  installer guidance, and `virtiofsd` capability checks

## v0.2.0 — 2026-04-18

### Features

- Merge pull request #12 from X-McKay/security/harden-virtiofsd-and-credential-docs (535fe23)
- feat: P1 install experience — ship v0.1.0 prerequisites (3e1ed7b)
- feat: add pre-release validation orchestrator (8b6025a)

### Fixes

- fix: replace map().unwrap_or(false) with is_ok_and() in kvm.rs (01068a7)
- fix: use is_ok_and instead of map().unwrap_or(false) for clippy (3c318f5)
- fix: address PR review feedback (5769c83)
- fix: correct REPO_ROOT paths after scripts/local/ restructure (008e3e6)
- fix: improve test robustness (5cbc887)
- fix(security): harden meta/status virtiofsd with namespace sandbox and seccomp (84b2420)

### Refactoring

- refactor: release.sh verifies attestation stamps instead of running tests (05facf1)

### Documentation

- docs: update pre-pr-checklist skill with tier vocabulary (863a0d0)
- docs: update release-preparation skill for attestation workflow (07e8d46)
- docs: update AGENTS.md with test tier system and pre-release workflow (7161f5f)
- docs: update pre-pr-checklist with tier vocabulary and release process (f83f474)
- docs: add pre-release validation spec and implementation plan (0cdf515)
- docs(security): add credential-scoping guide for least-privilege tokens (c3363f5)

### Chores

- chore: add tier recipes and pre-release entrypoint to justfile (160fe26)
- chore: update script references for scripts/local/ restructure (d0207ee)
- chore: move local-only test scripts to scripts/local/ (0cba691)

### Other

- Merge pull request #12 from X-McKay/security/harden-virtiofsd-and-credential-docs (535fe23)
- perf: reduce sandbox startup latency by 38% (478ms → 296ms) (faa3819)


All notable changes to abox are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## v0.1.0 — 2026-04-17

### Features

- feat: Codex (OpenAI) credential forwarding (#9) (a98fd27)
- feat(credential-forwarding): per-sandbox egress proxy with stub credentials and ADR-003 amendment (#4) (25a1ed1)
- feat(onboarding): add abox init/doctor, actionable errors, and doc cleanup (#3) (43d4405)
- feat: add one-command installer script (c955fea)
- feat(bootstrap): add --from-bundle flag for offline VM setup (0b3e214)
- feat: P3 snapshot/template fast startup (Steps 1-5) (3f35cc4)
- feat(cli): add `abox ca` subcommand for CA management (3ef1254)
- feat(policy): add cert-pinning bypass list for TLS passthrough (6292c07)
- feat(sandbox): per-sandbox egress port allocation and HTTPS_PROXY injection (c146da6)
- feat(proxy): implement header injection in MITM path (ca200b8)
- feat(proxy): add TLS-terminating MITM proxy core (978b1e4)
- feat(bench): add cold vs warm template startup benchmark (a75a1c2)
- feat(sandbox): wire template restore into sandbox creation (25ee638)
- feat(snapshot): save virtiofsd socket metadata for restore (27f17f6)
- feat(rootfs): bake CA cert into guest rootfs trust store (6f9b008)
- feat(vm): add StartMode enum and snapshot restore path in CH adapter (221822a)
- feat(ca): add root CA generation, persistence, and leaf cert signing with SNI cache (884bedc)
- feat(template): wire template create CLI to orchestrator (ec25862)
- feat: P2 stabilization release (N2-N7) (93fd10a)
- feat: parameterize bootstrap_vm.sh for aarch64 support (7a432dc)
- feat: add --timeout and --ephemeral flags to `abox run` (cc7fa4d)
- feat: add release workflow (just release <version>) (2f19186)
- feat: add benchmark suite — criterion microbenchmarks + VM latency script (e3fb4a0)
- feat(bootstrap): require --yes (or BOOTSTRAP_YES=1) to install rust targets (closes D3) (09130ba)
- feat(cli): add 'abox run --detach' (closes F1) (af6a5cd)
- feat(bootstrap): symlink VM binaries into ~/.local/bin by default (closes D1) (702644c)
- feat(console): graceful drain via shutdown signal (closes D5) (ff37d35)
- feat(vm): propagate guest agent exit code via aboxstatus share (7660592)
- feat(e2e): justfile recipes and gated phase 6 for full VM end-to-end (72d86b1)
- feat(cli): abox run is now a foreground VM supervisor (f220a82)
- feat(orchestrator): foreground run_sandbox with bridge + console streaming (f95654f)
- feat(vm): stage boot meta + console streaming + per-VM virtiofs share (26babf5)
- feat(core): add BootMeta type and agent_command field on VmConfig (0f3a4c5)
- feat(bootstrap): build static-musl shim and assemble guest rootfs (4e081fc)
- feat(bootstrap): download cloud-hypervisor, virtiofsd, kernel, alpine (9484256)
- feat(bootstrap): add vendor cache and bootstrap script skeleton (e06a6cd)
- feat: implement core abox architecture (b023856)
- feat: initial project scaffold with Cargo workspace, developer tooling, and implementation plan (189c68d)

### Fixes

- fix(guest): chmod proxy socket for non-root agent (#11) (378fd0a)
- fix(rootfs): pin CLI versions for reproducible builds (#10) (583a400)
- fix: non-root guest execution (ADR-004) (#8) (0c29809)
- fix(egress): refactor MITM injection to use hyper (fixes 401-mid-session) (#6) (560c69c)
- fix(credential-forwarding): address PR #4 review feedback (6 fixes) (#5) (93018a0)
- fix(ci): trigger workflow on label events so vm-attestation re-evaluates (1e0bc8d)
- fix(ci): correct VM-attestation path filter crate names (68ddfeb)
- fix(guest): set PATH in init and install bash/node/CLIs in rootfs (53fe85a)
- fix: resolve clippy warnings from P1 implementation (8d2dd27)
- fix(vm): track actual virtiofsd socket paths for correct cleanup (7b55876)
- fix: address P2 code review findings (4604643)
- fix: address P5 code review findings (5727ac9)
- fix: add stderr warning for silent VM failure (missing exit code) (1edad8f)
- fix: add SUN_LEN 108-byte socket path guard in CloudHypervisorAdapter::new() (50b36c8)
- fix(release): remove double 'ns' unit in criterion table rows (b336b4b)
- fix(boot_meta): export PATH in runner.sh so sub-shells find abox-shim (78a0904)
- fix(proxy,shim): honor forward_ssh_agent; prefer /proc/self/cwd (closes S3, S4) (0600339)
- fix(policy): strip git global options before allow/deny match (closes S1) (3beec80)
- fix: address final-review findings (console streaming, leaks, path match) (4347df5)
- fix: inline format args to satisfy clippy::uninlined_format_args (3fd212e)
- fix(bootstrap): trap-based temp cleanup + dedupe version pins (d963a3e)
- fix(bootstrap): preflight check for curl and sha256sum (810a7ad)
- fix: address e2e test report findings (issues #1-#7) (a1154c8)

### Refactoring

- fix: non-root guest execution (ADR-004) (#8) (0c29809)
- fix(egress): refactor MITM injection to use hyper (fixes 401-mid-session) (#6) (560c69c)
- refactor(core): lift VM runtime timing into VmRuntimeTuning (closes D2) (1426089)
- refactor(core): extract reusable proxy_bridge from proxyd cli_proxy (1a9ae44)
- refactor: code quality overhaul — naming, shared types, tooling, docs, agent skills (5d4ec4f)

### Testing

- test(integration): set repo-local git user in setup_test_repo (e777a72)
- test(e2e): harden default-deny test and surface cargo test failures (62d8a55)
- test(e2e): add credential injection test with real upstream (ad34cdf)
- test(e2e): add --detach lifecycle integration test in phase 6 (d77b31b)
- test: add CWD resolution chain tests for abox-shim (6685e60)
- test: add silent VM failure test for missing exit-code path (5c91042)
- test(e2e): add phase 7 — full agent lifecycle (commit, diverge, deny, merge) (68e678a)
- test(e2e): register cleanup trap on INT/TERM; sweep stale scratch dirs (closes H3) (df475be)
- test(e2e): assert guest console banner reached host stdout (closes D4) (836add6)
- test: add end-to-end shell test script (4988500)
- test: comprehensive test suite — 56 tests across all components (4d8a0c2)

### CI

- ci: add advisory doc-staleness reminder job (9e8861f)
- ci: add vm-attestation job (requires label for VM-path PRs) (417fb74)
- ci: add cargo-deny job for supply-chain audit (e2d5107)
- ci: run just check instead of inlined cargo invocations (bcaae28)
- ci(release): add GitHub Actions release workflow (c804058)
- ci: add GitHub Actions workflow for fmt/clippy/test + e2e phases 1-5 (closes D6) (43d4aae)

### Documentation

- docs(testing): add soak-test prompt for post-hyper-MITM validation (#7) (41f9622)
- Merge pull request #2 from X-McKay/docs/release-pipeline-spec (01dd84e)
- docs(spec): clarify §2.3 — just check + parallel cargo-deny (f6310aa)
- ci: add cargo-deny job for supply-chain audit (e2d5107)
- docs(agents): add branching, pre-PR, and doc-update rules (7d9533b)
- docs: add rollback procedure for shipped releases (d844238)
- docs(contributing): add branching convention and PR lifecycle (a4fe15f)
- docs(contributing): add canonical pre-PR checklist (2dca20b)
- docs: add implementation plan for release pipeline & dev ergonomics (94b4263)
- docs: add release pipeline and dev ergonomics design spec (980276d)
- docs: update README, explainer, and future-work after P1-P5 landing (ca68c5c)
- docs: add ADR-003 for HTTPS credential injection, update README and explainer (239aa1c)
- docs: add MIT license file (044e022)
- docs: add forward-looking roadmap (docs/future-work.md) (5827c54)
- docs: add ELI5 architecture explainer (4f883fa)
- docs: add 10-minute tutorial walkthrough (dfe0c74)
- docs(plans): deferred spec for HTTPS credential injection (F3) (01447ac)
- docs(backlog): summarize vm-e2e-hardening outcomes at top of backlog file (cf06f00)
- docs: README + vm-setup + CONTRIBUTING + ADR-002 + plan retrospective (7cda252)
- feat(bootstrap): symlink VM binaries into ~/.local/bin by default (closes D1) (702644c)
- docs(plans): add VM end-to-end hardening implementation plan (dbeb95b)
- docs: capture VM MVP follow-ups in docs/backlog/ (bedd530)
- docs: add vm-setup walkthrough and link from README (29b4371)
- docs: add VM end-to-end MVP implementation plan (fd7a2c9)

### Style

- style(sandbox): use if-let instead of match for single-pattern destructure (becc97d)
- style: rustfmt reformat proxy_bridge.rs (58852a3)

### Chores

- chore(cargo): mark internal workspace crates as publish = false (2425c56)
- chore(deny): add documented exemptions for current workspace findings (a92f511)
- chore(deny): migrate deny.toml to cargo-deny v2 schema (a505489)
- chore: replace MIT LICENSE with Apache-2.0 to match Cargo.toml (217befb)
- chore: harmonize formatting with rustfmt 1.8.0 (6d3a6f3)
- chore(github): add PR template with checklist acknowledgment (6d05149)
- chore(skills): add start-feature skill (ce68288)
- chore(skills): add rootfs-awareness skill (172a7f1)
- chore(skills): add release-preparation skill (e2ad043)
- chore(skills): add pre-pr-checklist skill (65bf2b1)
- chore(agents): add Claude Code pointer stub at .claude/AGENTS.md (b7e3a71)
- chore(agents): promote .claude/AGENTS.md to repo-root AGENTS.md (f2adb72)
- chore: ignore worktree directories (7ebdc1d)
- chore: update Cargo.lock after P3 merge (60da24b)
- chore: defer CI workflow (requires manual push with workflow permissions) (c1a249c)

### Other

- Merge pull request #2 from X-McKay/docs/release-pipeline-spec (01dd84e)
- Merge P4 installation story into main (0e1dd45)
- Merge P1 credential injection into main (0a60426)
- Merge P5 runtime controls into main (868a5e8)
- Merge agent/add-license into main (06fe369)
- Merge vm-e2e-hardening: VM end-to-end MVP + hardening pass (27c4465)
- Merge branch 'develop': core abox + fixes + e2e foundation (bb41a49)
- Initial commit (bb9dda5)

