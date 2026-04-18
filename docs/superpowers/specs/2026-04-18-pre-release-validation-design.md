# Pre-Release Validation Workflow Design

## Context

abox has three tiers of verification that must all pass before cutting a release, but they're currently run ad-hoc with no single entrypoint:

- `just check` (fmt + clippy + cargo test, 169 tests, ~5s, no KVM, runs in CI)
- `just e2e-vm` / `scripts/e2e_test.sh` (47 assertions across 7 phases, ~10s, requires KVM + bootstrapped VM artifacts, not in CI)
- `scripts/agent_smoke_test.sh` (7 tests exercising real Claude Code and Codex API calls through the MITM proxy, requires real OAuth credentials, ~60s, costs real API tokens, cannot run in CI)
- `just bench` + `just bench-vm` (criterion microbenchmarks + real VM latency benchmarks, results should be compared against previous release baseline)

Additionally, `scripts/release.sh` is the 12-step release orchestrator. It runs `just check` and e2e internally but does NOT run agent smoke tests or compare benchmarks against baselines.

### Problems

1. No single command to run everything needed before a release.
2. No clear separation between CI-safe and local-only tests — future contributors could accidentally wire credential-requiring scripts into GitHub Actions.
3. No benchmark regression detection against previous release baselines.
4. `release.sh` can't enforce that agent smoke tests were run.
5. No rootfs staleness check before VM tests (a recently changed `guest/init.sh` requires a rootfs rebuild).
6. The `scripts/` directory is flat with no naming convention distinguishing CI-safe from local-only.

## Design

### Test Tiers

Four tiers, ordered by cost and requirements:

| Tier | Name | What Runs | Requires | Cost | Time |
|------|------|-----------|----------|------|------|
| 1 | `ci` | `just check` (fmt + clippy + test) + `cargo deny` | Nothing special | Free | ~5s |
| 2 | `vm` | `scripts/local/e2e_test.sh` (all phases) + rootfs staleness check | `/dev/kvm` + bootstrapped VM artifacts | Free | ~10s |
| 3 | `bench` | `scripts/local/bench.sh` (VM latency, 5 runs) + `cargo bench` (criterion) + baseline comparison | `/dev/kvm` + bootstrapped VM artifacts | Free | ~30s |
| 4 | `smoke` | `scripts/local/agent_smoke_test.sh` (Claude + Codex) | KVM + real OAuth credentials | Real API tokens | ~60s |

### Directory Restructure

```
scripts/
├── ci/                          # CI-safe — no KVM, no credentials
│   └── README.md               # explains the ci/ vs local/ convention
├── local/                       # Requires KVM and/or credentials
│   ├── e2e_test.sh             # moved from scripts/
│   ├── agent_smoke_test.sh     # moved from scripts/
│   ├── bench.sh                # moved from scripts/
│   └── bench_template.sh       # moved from scripts/
├── lib/
│   └── download.sh             # stays
├── pre_release.sh              # new — orchestrator
├── release.sh                  # stays (updated to check attestations)
├── bootstrap_vm.sh             # stays (setup, not a test)
├── build_rootfs.sh             # stays (setup, not a test)
└── install.sh                  # stays (distribution, not a test)
```

The split: `ci/` is safe to run anywhere (including GitHub Actions). `local/` requires KVM, credentials, or both — never wired into CI workflows. Setup scripts (`bootstrap_vm.sh`, `build_rootfs.sh`, `install.sh`) stay at the top level since they're not tests.

### Justfile Recipes

```just
# Run all pre-release validation tiers, attest what passes.
pre-release:
    ./scripts/pre_release.sh

# Individual tiers (useful during development)
tier-ci: check deny
tier-vm: check-rootfs e2e-vm
tier-bench: bench bench-vm-n "5"
tier-smoke:
    ./scripts/local/agent_smoke_test.sh
```

Existing recipes (`check`, `deny`, `e2e-vm`, `bench`, `bench-vm-n`) remain as-is. The `tier-*` recipes compose them. The `e2e-vm` recipe path updates from `./scripts/e2e_test.sh` to `./scripts/local/e2e_test.sh`.

### Attestation Stamps

**Location:** `.abox-attestations/` at the repo root, `.gitignore`d.

**Three stamp files** (tiers 2, 3, 4 — tier 1 doesn't need attestation since CI runs it on every push):

```
.abox-attestations/
├── vm.json        # tier 2: e2e VM tests
├── bench.json     # tier 3: benchmarks
└── smoke.json     # tier 4: agent smoke tests
```

**Stamp format:**

```json
{
  "tier": "vm",
  "timestamp": "2026-04-18T15:30:00Z",
  "git_sha": "abc1234",
  "git_dirty": false,
  "result": "pass",
  "summary": "47/47 assertions passed (phases 1-7)",
  "hardware": {
    "arch": "x86_64",
    "cores": 32,
    "kernel": "6.14.0-37-generic"
  }
}
```

The bench stamp additionally includes comparison data:

```json
{
  "tier": "bench",
  "timestamp": "2026-04-18T15:30:30Z",
  "git_sha": "abc1234",
  "git_dirty": false,
  "result": "pass",
  "summary": "all metrics within threshold",
  "hardware": {
    "arch": "x86_64",
    "cores": 32,
    "kernel": "6.14.0-37-generic"
  },
  "baseline_version": "v0.1.0",
  "comparison": {
    "vm_boot_ms": {"baseline": 186, "current": 192, "delta_pct": 3.2},
    "full_run_ms": {"baseline": 478, "current": 485, "delta_pct": 1.5},
    "cleanup_ms": {"baseline": 17, "current": 18, "delta_pct": 5.9}
  },
  "regression_detected": false,
  "hardware_match": true
}
```

**Validity rules** (checked by `release.sh`):

1. `git_sha` must match current HEAD.
2. `git_dirty` must be `false`.
3. `result` must be `"pass"`.
4. All three stamps must be present.

If you commit new code after attestation, the SHA won't match and `release.sh` will tell you to re-run `just pre-release`.

### Orchestrator (`scripts/pre_release.sh`)

**Flow:**

```
pre_release.sh
  ├── 1. Preflight: clean tree check, detect capabilities
  │     ├── has_kvm?        (/dev/kvm accessible)
  │     ├── has_vm_bootstrap? (~/.abox/vm/cloud-hypervisor + rootfs.raw)
  │     ├── rootfs_fresh?   (just check-rootfs)
  │     ├── has_claude?     (~/.claude/.credentials.json exists)
  │     └── has_codex?      (~/.codex/auth.json exists)
  │
  ├── 2. Print capability matrix
  │     "KVM: Y  VM bootstrap: Y  rootfs fresh: Y  Claude creds: Y  Codex creds: N"
  │     "Will run: tier-ci, tier-vm, tier-bench, tier-smoke (claude only)"
  │
  ├── 3. Run tiers in order, continue on failure
  │     ├── just tier-ci        → always runs
  │     ├── just tier-vm        → requires KVM + bootstrap + fresh rootfs
  │     ├── just tier-bench     → requires KVM + bootstrap (runs after vm passes)
  │     └── just tier-smoke     → requires KVM + bootstrap + at least one credential set
  │
  ├── 4. Benchmark comparison (if tier-bench passed)
  │     ├── Find latest benchmarks/v*.json as baseline
  │     ├── Compare current results against baseline
  │     ├── Print delta table
  │     └── Flag regression if >15% on any metric AND hardware matches
  │
  ├── 5. Write attestation stamps (for each tier that passed)
  │     └── .abox-attestations/{vm,bench,smoke}.json
  │
  └── 6. Print summary report
        ├── Per-tier: PASS / FAIL / SKIPPED (with reason)
        ├── Benchmark deltas (if ran)
        ├── Attestation stamps written
        └── Exit code: 0 if all runnable tiers passed, 1 if any failed
```

**Key behaviors:**

- **Fail-fast within a tier, continue across tiers.** If `just tier-ci` fails, it's recorded as failed but the script still runs `tier-vm` etc. You see all failures in one run, not one at a time.
- **Tier ordering is deliberate.** CI first (cheapest/fastest), then VM tests, then benchmarks (only meaningful if VM tests pass), then smoke tests (most expensive). If VM tests fail, benchmarks are skipped (they'd be meaningless) but smoke tests still run since they exercise a different path. Note: this dependency is enforced by `pre_release.sh`, not by the just recipes — `just tier-bench` can be run standalone during development without requiring `tier-vm` to pass first.
- **Rootfs staleness blocks VM tiers.** If `just check-rootfs` fails, tiers 2-4 are all skipped with a message telling you to run `just rebuild-rootfs`.
- **Dirty tree blocks attestation.** The script runs fine on a dirty tree (useful during development) but stamps are only written if the tree is clean and all runnable tiers passed.

### Benchmark Comparison Logic

**Baseline resolution:** Find the latest `benchmarks/v*.json` by semantic version (not mtime).

**Comparison algorithm:**

```
for each metric in [vm_boot_ms, proxy_roundtrip_ms, full_run_ms, cleanup_ms]:
    baseline_avg = baseline.results[metric].avg
    current_avg  = current_results[metric].avg
    delta_pct    = ((current_avg - baseline_avg) / baseline_avg) * 100

    if delta_pct > 15 and hardware_match:
        → REGRESSION (stamp records regression_detected=true)
    elif delta_pct > 15 and not hardware_match:
        → ADVISORY WARNING (printed, does not fail)
    elif delta_pct < -15:
        → IMPROVEMENT (noted in output)
    else:
        → OK
```

**Hardware matching:** Compare `arch` and `cores` between baseline and current. Kernel version is recorded but not compared — minor kernel updates shouldn't cause regressions, and requiring an exact match would be too strict. If arch or core count differs, `hardware_match` is false and all regressions become advisory.

**Regression threshold:** 15% on any single metric when hardware matches.

**Output during pre_release.sh run:**

```
--- benchmark comparison (baseline: v0.1.0) ---
  hardware: x86_64/32 cores (matches baseline)

  METRIC              BASELINE    CURRENT     DELTA
  ------------------- ---------- ---------- ----------
  vm_boot_ms          186 ms      192 ms      +3.2%
  proxy_roundtrip_ms  186 ms      188 ms      +1.1%
  full_run_ms         478 ms      485 ms      +1.5%
  cleanup_ms           17 ms       18 ms      +5.9%

  OK: no regressions (threshold: 15%)
```

**Criterion microbenchmarks** (policy eval, serialization, boot meta) are run as part of `tier-bench` but not compared against a stored baseline. Criterion has its own built-in comparison against the last run in `target/criterion/`. The pre-release script just checks that `cargo bench` exits 0.

**Benchmark data flow:**

1. `bench.sh` produces JSON to stdout.
2. `pre_release.sh` captures it, runs comparison, writes `bench.json` attestation stamp (includes current results + comparison).
3. `release.sh` reads the attestation stamp to update the README benchmark table.
4. `release.sh` saves a clean copy to `benchmarks/v<version>.json` for the next release's baseline.

### `release.sh` Integration

**New step 2:** Verify attestation stamps. Steps shift from 12 to 13.

```
[1/13] Preflight checks...
  OK: clean tree, tag v0.2.0 available

[2/13] Verifying attestations...
  checking .abox-attestations/vm.json
    OK: sha=abc1234 matches HEAD, passed 2026-04-18T15:30:00Z
  checking .abox-attestations/bench.json
    OK: sha=abc1234 matches HEAD, no regressions
  checking .abox-attestations/smoke.json
    OK: sha=abc1234 matches HEAD, passed 2026-04-18T15:31:00Z

[3/13] Bumping version...
```

**What release.sh stops doing itself:**

- Running `cargo fmt --check + clippy + test` (covered by tier-ci attestation).
- Running `scripts/e2e_test.sh` (covered by tier-vm attestation).
- Running VM benchmarks (covered by tier-bench attestation).

**What release.sh keeps doing:**

- Version bump.
- Release build (still needed to produce the tagged binary).
- README benchmark table update — reads from the bench attestation stamp instead of running benchmarks inline.
- Save benchmark JSON to `benchmarks/v<version>.json` — copies from attestation data.
- Changelog generation (unchanged).
- `cargo install` (unchanged).
- Commit + tag (unchanged).

**Escape hatch:**

```bash
./scripts/release.sh 0.2.0 --skip-attestation
```

Prints a loud warning but proceeds. For emergencies where you need to cut a release and can't re-attest. Should never be routine.

### Test Robustness Improvements

**Agent smoke tests — Codex retry:**

Codex tests (C1, C2) parse raw text output via long grep exclusion lists because Codex lacks `--output-format json`. Add retry-once-on-failure with a 5-second pause to handle transient API flakiness. Tighten timeout from 90s to 60s for single-turn Codex calls.

**E2e test — proxyd startup race:**

`e2e_test.sh:306-309` polls for `cli-proxy.sock` with `sleep 0.05` in a loop (max ~2s). Replace with `inotifywait` if available, fall back to the existing poll loop.

**E2e test — phase 6 VM timeout:**

Tighten the 90-second timeout on VM operations to 30s. Benchmarks show `full_run_ms` at ~480ms — 30s is 60x that, generous enough to avoid flakiness while catching hangs faster.

### Documentation Updates

**`AGENTS.md`:**

- Add a "Test Tiers" section explaining the four tiers, their requirements, and when to use each.
- Update script paths after directory restructure.
- Add a "Release Process" subsection: `just pre-release` then `just release`.
- Update the meta-rule about tooling changes requiring doc updates to include `scripts/pre_release.sh`.

**`.claude/skills/release-preparation.md`:**

- Add `just pre-release` as a required precondition before `just release`.
- Document the attestation stamp requirement.
- Update the step list to reflect that `release.sh` becomes 13 steps (new step 2: verify attestations) and no longer runs tests/benchmarks itself.
- Update the "Do not" section: don't skip `just pre-release`, don't use `--skip-attestation` routinely.

**`.claude/skills/pre-pr-checklist.md`:**

- Reference the tier system and `just tier-ci` / `just tier-vm` as the vocabulary.
- Update the e2e script path from `./scripts/e2e_test.sh` to `./scripts/local/e2e_test.sh`.
- Note that `just pre-release` is for release prep, while `just tier-ci` + `just tier-vm` are the PR-level gates.

**`docs/contributing/pre-pr-checklist.md`:**

- Update as the canonical source for the above path and vocabulary changes. The skill walks this document, so it must be updated first.

## Critical Files

### New
- `scripts/pre_release.sh` — orchestrator
- `.abox-attestations/` — stamp directory (`.gitignore`d)
- `scripts/ci/` — CI-safe test directory (initially empty)
- `scripts/local/` — local-only test directory

### Modified
- `justfile` — add `pre-release`, `tier-ci`, `tier-vm`, `tier-bench`, `tier-smoke` recipes; update `e2e-vm` path
- `scripts/release.sh` — add attestation verification step, remove inline test/benchmark execution, add `--skip-attestation` flag
- `.gitignore` — add `.abox-attestations/`
- `AGENTS.md` — test tier documentation, updated paths, release process
- `.claude/skills/release-preparation.md` — pre-release attestation requirement
- `.claude/skills/pre-pr-checklist.md` — tier vocabulary, updated paths
- `docs/contributing/pre-pr-checklist.md` — canonical source updates

### Moved
- `scripts/e2e_test.sh` → `scripts/local/e2e_test.sh`
- `scripts/agent_smoke_test.sh` → `scripts/local/agent_smoke_test.sh`
- `scripts/bench.sh` → `scripts/local/bench.sh`
- `scripts/bench_template.sh` → `scripts/local/bench_template.sh`

### Robustness Fixes
- `scripts/local/agent_smoke_test.sh` — Codex retry-once, tighten timeout
- `scripts/local/e2e_test.sh` — `inotifywait` for proxyd socket, tighten VM timeout to 30s

## Verification

1. `just pre-release` on a fully-equipped machine (KVM + credentials) runs all four tiers, prints the capability matrix, benchmark comparison table, and summary. All three attestation stamps are written.
2. `just pre-release` on a machine without credentials runs tiers 1-3, skips tier 4 with a clear message, writes `vm.json` and `bench.json` stamps only.
3. `just pre-release` on a machine without KVM runs tier 1 only, skips tiers 2-4, writes no stamps.
4. `just release <version>` with valid stamps at HEAD succeeds.
5. `just release <version>` with missing or stale stamps fails with an actionable message.
6. `just release <version> --skip-attestation` succeeds with a loud warning.
7. `just tier-vm` works standalone during development.
8. CI workflows (`ci.yml`) continue to pass — no paths broken by the directory restructure.
9. Benchmark regression >15% on matching hardware is detected and flagged in the bench stamp.
10. Benchmark regression >15% on non-matching hardware is printed as advisory, does not fail.
