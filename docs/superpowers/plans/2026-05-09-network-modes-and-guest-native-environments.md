# Network Modes and Guest-Native Environments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a simple, explicit product surface for sandbox network intent (`safe`, `scoped`, `open`), prompt-file input for first-class managed agents, repo-owned sandbox defaults, and guest-native environment reuse through durable caches and prepare flows without weakening abox's current host-secret and VM-isolation guarantees.

**Architecture:** Build this in small layers. First add strict repo config parsing and a resolved per-run model. Then compile network modes into the existing host-mediated egress path with explicit host-policy coexistence and mode-driven transport selection. Then add trust-on-first-use and prompt delivery adapters for known managed agents. Only after that foundation lands, add guest-native caches and prepare flows. Snapshot-backed warm reuse is a separate spike and must not be assumed by the mainline rollout.

**Tech Stack:** Rust (`abox-core`, `abox-cli`, `abox-proxyd`), TOML (`serde`, `toml`), existing HTTPS MITM / CONNECT proxy path, existing boot metadata staging, existing snapshot/template support, guest init shell.

**ADR:** `docs/decisions/007-network-modes-and-guest-native-environments.md`

**Spec:** `docs/superpowers/specs/2026-05-09-network-modes-and-guest-native-environments-design.md`

---

## Release Slicing

This design should not land as one opaque branch.

### Slice A1: Network modes and minimal repo config

Land first:

- `.abox/project.toml`
- `safe` / `scoped` / `open`
- network bundles + hostname additions
- explicit host-policy coexistence
- `abox project init/validate`

### Slice A2: Trust-on-first-use

Land second:

- approval fingerprinting
- approval store
- launch-time approval checks
- immutable staging of approved prompt / prepare inputs
- `abox project trust/explain`

### Slice A3: Prompt inputs for known managed agents

Land third:

- `--prompt`
- `--prompt-file`
- repo default prompt file
- concrete launch adapters for bare `claude` and bare `codex`

### Slice B: Guest-native caches and prepare flows

Land fourth:

- durable per-project caches
- `abox env status/warm/reset/prune`
- cache lifecycle visibility
- prepare execution inside the real guest

### Slice C: Snapshot-backed warm reuse spike

This is not part of the first release claim by default.

Deliver only if the spike proves that snapshot restore can be retargeted cleanly
to:

- a fresh worktree
- fresh boot metadata
- fresh status reporting paths

If the spike fails, keep the release claim at cache-backed setup acceleration.

### Slice D: Official guest profiles

This is the next expansion step after the validated base release.

Land only as a narrow, curated profile model:

- `base`
- `node`
- `python`
- `rust`

Do not expose arbitrary guest image selection in repo config.

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/abox-core/src/project.rs` | Create | Repo config schema, validation, identity, approval/env fingerprints |
| `crates/abox-core/src/lib.rs` | Modify | Export new `project` module |
| `crates/abox-core/src/policy.rs` | Modify | Network mode model, bundle catalog, effective egress compilation |
| `crates/abox-core/src/egress.rs` | Modify | Transport and allowed-port decisions for managed vs passthrough traffic |
| `crates/abox-core/src/boot_meta.rs` | Modify | Prompt staging metadata and immutable staged-input support |
| `crates/abox-core/src/sandbox.rs` | Modify | Repo config load, approval checks, resolved run settings, env lifecycle |
| `crates/abox-core/src/snapshot.rs` | Modify | Spike-only work for snapshot-retarget feasibility |
| `crates/abox-core/src/adapters/cloud_hypervisor.rs` | Modify | Extra virtiofs share(s) for caches; spike-only snapshot restore updates |
| `crates/abox-core/tests/integration_tests.rs` | Modify | End-to-end validation for network modes, prompt sources, env reuse |
| `crates/abox-proxyd/src/egress_proxy.rs` | Modify | Enforce compiled transport mode / allowlist decisions |
| `crates/abox-proxyd/tests/proxy_tests.rs` | Modify | Proxy-level tests for mode enforcement and passthrough behavior |
| `crates/abox-cli/src/main.rs` | Modify | Register new subcommands and flags |
| `crates/abox-cli/src/commands/mod.rs` | Modify | Export new command modules |
| `crates/abox-cli/src/commands/run.rs` | Modify | `--network`, `--prompt`, `--prompt-file`, prompt adapters, trust/error UX |
| `crates/abox-cli/src/commands/init.rs` | Modify | Scaffold `.abox/project.toml` and helper files |
| `crates/abox-cli/src/commands/doctor.rs` | Modify | Report installed official guest profiles and profile-specific gaps |
| `crates/abox-cli/src/commands/project.rs` | Create | `init`, `validate`, `trust`, `explain`, `add-bundle`, `add-domain` |
| `crates/abox-cli/src/commands/env.rs` | Create | `status`, `warm`, `reset`, `prune` |
| `guest/init.sh` | Modify | Guest-side cache mount / bind setup and staged prepare execution |
| `scripts/build_rootfs.sh` | Modify | Build official `base` / `node` / `python` / `rust` profile images |
| `policies/default.toml` | Modify | Keep managed provider rules as the base secure surface |

---

## Task 1: Add strict repo config and resolved project model

**Files:**

- Create: `crates/abox-core/src/project.rs`
- Modify: `crates/abox-core/src/lib.rs`
- Modify: `crates/abox-core/tests/integration_tests.rs`

- [ ] **Step 1: Add the repo config schema with `deny_unknown_fields`**

Create a new `project.rs` module that owns `.abox/project.toml` parsing and
validation. The initial public types should include:

- `ProjectConfig`
- `ProjectSection`
- `NetworkConfig`
- `EnvironmentConfig`
- `AgentConfig`
- `NetworkMode`
- `EnvironmentProfile` (follow-up Slice D)

Use `serde(deny_unknown_fields)` on every repo-config struct so typos fail
closed.

- [ ] **Step 2: Use the new network field names**

The repo schema should use:

```toml
[network]
mode = "scoped"
bundles = ["npm-public"]
domains = ["docs.rs"]
```

Do not use `allow = [...]`; reserve clearer, user-facing terminology now.

- [ ] **Step 3: Add load, locate, normalize, and validate helpers**

Add helpers such as:

- `ProjectConfig::default_path(repo_root: &Path) -> PathBuf`
- `ProjectConfig::load(repo_root: &Path) -> Result<Option<ProjectConfig>>`
- `ProjectConfig::normalize(&self) -> ResolvedProjectConfig`
- `ProjectConfig::validate(&self, repo_root: &Path) -> Result<()>`

Validation must reject:

- unknown bundle names
- malformed hostnames
- `safe` with non-empty scoped additions
- missing referenced prompt file
- missing referenced prepare script

Normalization should convert `scoped` with no additions into `safe` and record
an explanation message rather than preserving a redundant config shape.

- [ ] **Step 4: Add project identity and fingerprint helpers**

Add helpers for:

- automatic project identity
- optional `[project].id` override
- approval fingerprint
- environment fingerprint input collection

For the first version, the approval fingerprint must cover:

- `.abox/project.toml`
- referenced default prompt file contents, if any
- referenced prepare script contents, if any
- bundle catalog version or resolved host set

- [ ] **Step 5: Add targeted unit tests**

Cover:

- strict parse success and failure
- contradiction detection
- missing-file errors
- identity derivation fallback rules
- stable fingerprint generation
- `scoped` normalization to `safe`
- profile/cache compatibility rules when Slice D lands

---

## Task 2: Compile `safe` / `scoped` / `open` into an effective egress model

**Files:**

- Modify: `crates/abox-core/src/policy.rs`
- Modify: `crates/abox-core/src/egress.rs`
- Modify: `crates/abox-proxyd/src/egress_proxy.rs`
- Modify: `crates/abox-proxyd/tests/proxy_tests.rs`

- [ ] **Step 1: Introduce an explicit effective network model**

Add a compiled representation that can answer:

- is the destination allowed?
- is it host-managed or unmanaged?
- should it use MITM or passthrough?
- what source allowed it? (`safe`, bundle, explicit domain, `open`, advanced host policy)
- is the port allowed?

- [ ] **Step 2: Add a built-in network bundle catalog**

Implement the initial curated catalog in Rust, not repo-authored TOML. Include:

- `npm-public`
- `pypi-public`
- `cargo-public`

Each bundle should carry:

- hostnames it adds
- default transport class (`passthrough`)
- human-readable description for `project explain`
- a catalog version input for trust

- [ ] **Step 3: Define mode compilation rules**

Compile modes as follows:

- `safe`: built-in managed provider destinations plus surfaced advanced
  host-managed destinations
- `scoped`: `safe` plus bundle and explicit-domain additions
- `open`: broad HTTPS/web API destination scope on standard CONNECT traffic

Important: `open` should remain "proxy-mediated HTTPS egress", not raw guest
networking and not an arbitrary host:port tunnel.

- [ ] **Step 4: Make host-policy coexistence explicit**

The effective decision model must preserve:

- managed provider rules from `policies/default.toml`
- host-owned advanced managed egress rules
- existing `bypass_tls` transport overrides

Repo config should widen unmanaged scope in structured ways, not replace the
host policy file.

- [ ] **Step 5: Add proxy-level tests**

Add tests that prove:

- `safe` denies unmanaged public web destinations
- `scoped` allows declared bundle and explicit-domain additions
- `open` allows broad `443` passthrough web access
- non-standard ports are rejected by the simple mode layer
- managed provider destinations still use MITM in every mode
- host `bypass_tls` entries still win when a destination is otherwise allowed

---

## Task 3: Add trust-on-first-use for repo-owned behavior

**Files:**

- Modify: `crates/abox-core/src/project.rs`
- Modify: `crates/abox-core/src/sandbox.rs`
- Create: `crates/abox-cli/src/commands/project.rs`
- Modify: `crates/abox-cli/src/main.rs`
- Modify: `crates/abox-cli/src/commands/mod.rs`

- [ ] **Step 1: Create a local approval store under host state**

Store per-repo approval records under abox state, keyed by:

- project identity
- approval fingerprint

Keep the on-disk format simple and inspectable, for example one TOML or JSON
file per approved fingerprint.

- [ ] **Step 2: Stage immutable approved behavior inputs**

Before launch, stage immutable copies of:

- resolved prompt content
- prepare script content

Launch must consume those staged bytes, not reread the live worktree after the
approval check.

- [ ] **Step 3: Implement approval checks in `run_sandbox`**

Before launching a sandbox with repo-owned behavior, resolve and summarize:

- network mode
- bundles
- explicit domains
- advanced host-managed destinations
- prepare script path
- default prompt file path

If the current fingerprint is unapproved:

- on TTY: present a trust-on-first-use approval prompt
- off TTY: fail with an actionable message telling the operator to run
  `abox project trust` first

- [ ] **Step 4: Add `abox project trust` and `abox project explain`**

Implement:

- `abox project validate`
- `abox project trust`
- `abox project explain`

`project explain` should be plain-language output, not raw debug structs.

- [ ] **Step 5: Add tests for approval invalidation**

Cover:

- first-use approval required
- approval reuse when nothing changed
- approval invalidation when config changes
- approval invalidation when referenced prompt / prepare file contents change
- approval invalidation when the bundle catalog version changes

---

## Task 4: Add prompt inputs and concrete managed-agent delivery

**Files:**

- Modify: `crates/abox-cli/src/commands/run.rs`
- Modify: `crates/abox-core/src/boot_meta.rs`
- Modify: `crates/abox-core/src/sandbox.rs`
- Modify: `crates/abox-core/tests/integration_tests.rs`

- [ ] **Step 1: Extend `abox run` with explicit prompt flags**

Add:

- `--network <safe|scoped|open>`
- `--prompt <text>`
- `--prompt-file <path>`

Reject invalid combinations such as:

- both `--prompt` and `--prompt-file`
- prompt flags together with command shapes abox does not know how to adapt

Keep `--task` as the short sandbox identifier only.

- [ ] **Step 2: Resolve prompt content on the host**

Add a resolved prompt model in the `run` path:

- inline content
- file content
- repo default prompt file

The host should resolve the chosen content before VM launch so guest-side path
ambiguity does not exist.

- [ ] **Step 3: Stage prompt content for reproducibility**

Add resolved prompt content to boot metadata and stage it into a consistent
guest-owned location such as `/abox-meta/prompt.md`.

This staged file is for:

- inspection
- provenance
- approval reproducibility

It is not sufficient by itself as the delivery contract.

- [ ] **Step 4: Define the first-release launch adapters**

Make prompt delivery real for known managed agents:

- bare `claude` plus prompt input is rewritten into Claude's non-interactive
  query / print flow with the resolved prompt as the query text
- bare `codex` plus prompt input is rewritten into `codex exec`, with the
  resolved prompt passed directly or via stdin

If the user supplies a command shape abox cannot adapt safely, fail with a
clear error rather than staging a prompt no process consumes.

- [ ] **Step 5: Add tests**

Cover:

- CLI argument validation
- prompt precedence (`CLI > repo default`)
- prompt-file missing-path errors
- prompt staging in boot metadata
- `claude` adapter rewrite
- `codex` adapter rewrite

---

## Task 5: Add guest-native managed caches

**Files:**

- Modify: `crates/abox-core/src/project.rs`
- Modify: `crates/abox-core/src/sandbox.rs`
- Modify: `crates/abox-core/src/adapters/cloud_hypervisor.rs`
- Modify: `guest/init.sh`
- Modify: `crates/abox-core/tests/integration_tests.rs`
- Create / Modify: `crates/abox-cli/src/commands/env.rs`

- [ ] **Step 1: Define the first supported cache types**

Start with a small built-in set:

- `npm`
- `cargo`
- `pip`
- `uv`

Reject unknown cache types in repo config.

- [ ] **Step 2: Create a per-project cache root on the host**

Key caches by:

- project identity
- cache type

These caches should be long-lived and not invalidated by lockfile or normal
source changes.

- [ ] **Step 3: Mount cache state into the guest**

Plumb one or more extra virtiofs shares so the guest can see host-owned cache
roots.

Prefer one abox-managed cache share with per-ecosystem subdirectories over a
large number of independent mounts.

- [ ] **Step 4: Map caches to ecosystem-standard guest paths**

In `guest/init.sh`, bind or export the cache directories to the locations tools
already expect, such as:

- npm cache directory
- cargo registry and git cache
- pip / uv cache directories

This is where user experience matters most: users should not have to manually
reconfigure every tool to benefit.

- [ ] **Step 5: Add cache lifecycle visibility**

`abox env status` and `abox env prune` should show:

- cache types present
- total size on disk
- what prune would remove

Do not let long-lived caches become invisible hidden state.

- [ ] **Step 6: Add tests**

Cover:

- configured caches mount correctly
- unknown caches fail closed
- cache directories persist across fresh sandboxes for the same repo
- status output reports cache presence and size

---

## Task 6: Add prepare flows and explicit cache-priming UX

**Files:**

- Modify: `crates/abox-core/src/project.rs`
- Modify: `crates/abox-core/src/sandbox.rs`
- Create / Modify: `crates/abox-cli/src/commands/env.rs`
- Modify: `crates/abox-cli/src/main.rs`
- Modify: `crates/abox-cli/src/commands/mod.rs`

- [ ] **Step 1: Define the environment fingerprint inputs**

Include:

- selected environment profile
- guest/rootfs version or digest
- resolved repo config
- selected cache set
- prepare script contents
- relevant dependency lockfiles
- explicit `watch` file contents

Do not include ordinary source file edits.

- [ ] **Step 2: Add an environment status store**

Store per-project environment metadata in host state:

- last successful prepare fingerprint
- last warm time
- last failure summary, if any

- [ ] **Step 3: Implement `abox env status` and `abox env warm`**

`env warm` should:

- create or reuse the per-project cache mounts
- boot a sandbox in the real guest
- run the staged prepare script if configured
- leave durable caches warmer than before
- record status and failure/success metadata

Do not claim snapshot-backed persistence here.

- [ ] **Step 4: Implement `abox env reset` and `abox env prune`**

`reset` is repo-focused.
`prune` is global.

Both should operate on host-managed state without asking users to manually
delete hidden directories.

- [ ] **Step 5: Add tests**

Cover:

- stale detection when lockfiles or prepare script change
- no stale detection on ordinary source edits
- failed prepare does not mark warm state valid
- `env reset` clears environment metadata without corrupting durable caches

---

## Task 7: Spike snapshot-backed warm reuse before committing to it

**Files:**

- Modify: `crates/abox-core/src/snapshot.rs`
- Modify: `crates/abox-core/src/adapters/cloud_hypervisor.rs`
- Modify: `crates/abox-core/src/sandbox.rs`
- Modify: `crates/abox-core/tests/integration_tests.rs`

- [ ] **Step 1: Prototype restore against a fresh worktree**

Demonstrate whether a restored snapshot can safely boot with:

- a new workspace share
- new boot metadata
- new status reporting

The current snapshot model stores the original virtiofs socket layout and was
built for restoring "the same VM again", not a retargeted project environment.

- [ ] **Step 2: Decide go / no-go**

If the spike succeeds:

- add a follow-up implementation plan for template-backed reuse

If the spike fails:

- keep the release claim at cache-backed acceleration plus explicit prepare
  validation

Do not silently let this become hidden complexity inside Slice B.

---

## Task 8: Add user-facing scaffolding and editing helpers

**Files:**

- Modify: `crates/abox-cli/src/commands/init.rs`
- Create / Modify: `crates/abox-cli/src/commands/project.rs`
- Create / Modify: `crates/abox-cli/src/commands/env.rs`
- Modify: `crates/abox-cli/src/main.rs`
- Modify: `crates/abox-cli/src/commands/mod.rs`

- [ ] **Step 1: Update `abox init` to offer repo scaffolding**

Add optional scaffolding for:

- `.abox/project.toml`
- `.abox/prepare.sh`
- `.abox/prompt.md`

Keep the generated config minimal and mode-safe by default. The starter config
should prefer:

```toml
[network]
mode = "safe"
```

- [ ] **Step 2: Add narrow editing helpers**

Implement helper commands that modify repo config structurally instead of
requiring hand-edited low-level TOML for the common path:

- `abox project add-bundle <bundle>`
- `abox project add-domain <hostname>`

- [ ] **Step 3: Add explanation output**

Ensure at least one command gives a compact, high-signal explanation of the
effective behavior, for example:

- network mode
- allowed managed destinations
- advanced host-managed destinations
- added bundles
- explicit domains
- configured caches
- prepare script path
- default prompt file

This is critical for trust and AI-tool ergonomics.

- [ ] **Step 4: Keep the happy path obvious**

Make sure the docs and help text make this sequence easy to discover:

1. `abox project init`
2. `abox project add-bundle ...` or `abox project add-domain ...` if needed
3. `abox project trust`
4. `abox env warm` if the repo uses caches / prepare
5. `abox run --task ... --prompt-file ... -- codex`

---

## Task 9: Verification, docs, and release hardening

**Files:**

- Modify: `crates/abox-core/tests/integration_tests.rs`
- Modify: `crates/abox-proxyd/tests/proxy_tests.rs`
- Modify: `docs/testing/*.md` as needed
- Modify: `docs/decisions/007-network-modes-and-guest-native-environments.md` if the spike changes scope
- Modify: `docs/superpowers/specs/2026-05-09-network-modes-and-guest-native-environments-design.md` if implementation clarifies any details

- [ ] **Step 1: Add unit coverage for the new trust/config/network modules**

Minimum coverage:

- strict parsing
- contradiction detection
- bundle compilation
- fingerprint stability
- prompt precedence

- [ ] **Step 2: Add integration coverage for the user-facing flows**

Minimum flows:

- `safe` mode blocks public web access
- `scoped` mode allows declared registry/docs hostnames
- `open` mode allows broad passthrough `443` web access
- untrusted repo config blocks launch until trusted
- `--prompt-file` resolves on host and reaches the adapted managed-agent launch
- caches survive fresh sandboxes

- [ ] **Step 3: Add one full cache-warm smoke path**

Use a small fixture repo that has:

- one package-manager cache
- one prepare script
- one lockfile

Exercise:

1. cold cache warm-up
2. repeated run with warm cache
3. lockfile-triggered stale detection

- [ ] **Step 4: Re-run the existing release gate**

Before shipping, run the standard full validation suite plus the new focused
smokes for network modes, prompt adapters, and cache-backed setup flows.

---

## Notes and Guardrails

- Keep the repo config narrow. Do not let this plan slide back into generic
  repo-authored low-level policy.
- Keep managed secret-bearing behavior host-owned. Repo config may widen
  unmanaged scope; it must not define new credential injection rules.
- Prefer built-in bundle metadata over more TOML layers in the first release.
- Treat prompt-file support as an end-to-end launch feature for known agents,
  not as a guest-side file staging trick.
- Make `project explain` and cache status output part of the main product, not
  optional niceties. Legibility is a core security feature here.
- Do not claim snapshot-backed warm reuse unless the retargeting spike proves
  it against fresh worktrees and fresh metadata.

## Exit Criteria

This plan is complete for the mainline release when abox can honestly support
the following workflow:

1. A repo checks in `.abox/project.toml` with `safe`, `scoped`, or `open`.
2. A user or AI tool can review and trust that repo config explicitly.
3. `abox run --task <id> --prompt-file <file> -- codex` works through a
   concrete managed-agent adapter rather than by staging an unread file.
4. `scoped` mode can add common registry access or targeted hostnames without
   resorting to raw policy files.
5. A repo can declare durable caches and a prepare script, and abox can warm
   and reuse that cache-backed setup across fresh sandboxes with clear status
   and prune behavior.

Snapshot-backed prepared-environment reuse is explicitly outside the mainline
exit criteria unless the spike in Task 7 proves it supportable.

---

## Task 10: Add official environment profile selection

**Files:**

- Modify: `crates/abox-core/src/project.rs`
- Modify: `crates/abox-core/src/config.rs`
- Modify: `crates/abox-core/src/sandbox.rs`
- Modify: `crates/abox-cli/src/commands/project.rs`
- Modify: `crates/abox-cli/src/commands/env.rs`
- Modify: `crates/abox-cli/src/commands/run.rs`

- [ ] **Step 1: Add a narrow `EnvironmentProfile` enum**

Support only:

- `base`
- `node`
- `python`
- `rust`

Make `environment.profile` optional and default it to `base`.

- [ ] **Step 2: Keep profile choice repo-owned and simple**

Add repo config support such as:

```toml
[environment]
profile = "python"
caches = ["uv"]
prepare = ".abox/prepare.sh"
```

Do not expose raw guest image paths in `.abox/project.toml`.
Do not support profile composition in the first version.

- [ ] **Step 3: Add profile-aware validation and explanation**

Validation should fail closed when cache choices do not fit the selected
profile, for example:

- `npm` requires `node`
- `uv` or `pip` requires `python`
- `cargo` requires `rust`

`abox project explain` should surface:

- selected profile
- guaranteed toolchain
- configured caches
- prepare script path

- [ ] **Step 4: Include the selected profile in environment freshness**

Environment fingerprints and warm-state records must include:

- selected profile name
- selected profile image token/digest

Changing profile should invalidate warm state cleanly.

- [ ] **Step 5: Add user-facing repo helpers**

Keep the CLI narrow:

- `abox project init --profile <base|node|python|rust>`
- `abox project set-profile <base|node|python|rust>`

Avoid a broad per-run `--profile` override in the first version. One repo
should have one primary profile unless the user intentionally edits repo
config.

## Task 11: Build, install, and validate official guest profiles

**Files:**

- Modify: `scripts/build_rootfs.sh`
- Modify: `crates/abox-cli/src/commands/init.rs`
- Modify: `crates/abox-cli/src/commands/doctor.rs`
- Modify: `docs/vm-setup.md`
- Modify: `docs/tutorial.md`

- [ ] **Step 1: Define the host install layout**

Use a host-managed layout for official profile images, for example:

- `~/.abox/vm/profiles/base/rootfs.raw`
- `~/.abox/vm/profiles/node/rootfs.raw`
- `~/.abox/vm/profiles/python/rootfs.raw`
- `~/.abox/vm/profiles/rust/rootfs.raw`

Keep repo config selecting a profile name, not a path.

- [ ] **Step 2: Extend setup to install named profiles**

`abox init` should keep the common path simple:

- install `base` by default
- optionally install additional official profiles on request

Avoid requiring users to understand image internals.

- [ ] **Step 3: Add profile visibility to diagnostics**

`abox doctor` should report:

- which official profiles are installed
- which profile the current repo requests
- whether that requested profile is available locally

- [ ] **Step 4: Add toolchain preflight**

Before `env warm` or `run` relies on a profile-specific workflow, fail fast
with an actionable error if:

- the selected profile image is missing
- the guest profile is missing the expected toolchain

- [ ] **Step 5: Add a profile smoke matrix**

At minimum validate:

- `node` with `npm`
- `python` with `uv`
- `rust` with `cargo`

The branch should not claim profile support for an ecosystem until the real
guest image and warm flow have been exercised end-to-end.
