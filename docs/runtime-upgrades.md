# MicroSandbox runtime upgrades

The MicroSandbox host runtime is part of abox's trusted computing base
([`security-model.md`](security-model.md)): the `msb` process and libkrun sit
under every sandbox. Its version is therefore **pinned exactly** and updated
only through a dedicated, qualified dependency PR — never as a side effect of
another change (ADR-008).

## The pin

- `crates/abox-core/Cargo.toml` pins `microsandbox = "=0.6.9"` (and
  `microsandbox-network = "=0.6.9"` — keep the two in lockstep).
- The runtime *assets* (`msb` binary + libkrunfw under `$MSB_HOME`) are
  installed by `abox init` via the SDK, so their version follows the crate
  pin.

## Upgrade procedure

One PR, containing only the runtime bump and any adaptation it forces:

1. **Review upstream first.** Read the MicroSandbox release notes and any
   security advisories between the current pin and the target version. Note
   changes to vsock routing, network isolation, secret substitution, image
   handling, and the exec API — those are the surfaces abox's security
   semantics ride on.
2. **Bump the pin** in `crates/abox-core/Cargo.toml` (both crates) and
   `Cargo.lock`. Keep the `=` exact-version requirement.
3. **Qualify against the runtime contract:**
   - `just tier-ci` — fmt + clippy + tests + supply-chain audit (the
     compiled-policy SSRF/network-plan invariants are unit release gates
     here).
   - `just e2e-runtime` — the live MicroSandbox suite
     (`scripts/local/msb_e2e_test.sh`) against real microVMs: exit-code
     propagation, workspace write-through and isolation, brokered
     `git`/policy-deny/audit attribution, and host-mediated HTTPS egress.
   - The security-relevant suites for anything the release notes flag
     (network modes live validation, credential isolation, audit
     attribution).
4. **Known-quirk regression checks.** The current adapter works around two
   VMM-level vsock behaviors observed in 0.6.9 (see the module docs in
   `crates/abox-core/src/adapters/microsandbox.rs`):
   - **vsock half-close is not propagated** guest↔host;
   - **rapid per-process reconnects** to a recently used vsock port are
     reset — which is why the command broker multiplexes over one persistent
     `abox-bridge` uplink.

   On every upgrade, confirm the e2e suite's repeated-broker-call assertions
   still pass. If upstream fixes either quirk, keep the workaround until it
   is deliberately removed in its own change — the persistent-uplink design
   is also load-bearing for attribution and retry semantics.
5. **Open the PR** with the upstream review summarized in the description,
   and attest it: runtime paths are touched, so the `runtime-attested` label
   and a timestamped `just e2e-runtime` comment are required
   ([`contributing/pre-pr-checklist.md`](contributing/pre-pr-checklist.md)).

## What an upgrade must never do silently

- Change what `safe`/`scoped`/`open` mean. The plans are compiled by abox
  policy; if a new runtime version cannot represent a plan exactly, abox
  must fail closed at launch, not degrade.
- Widen a credential rule. Native secret substitution stays limited to the
  rule shapes `native_substitution` validates; everything else stays on the
  abox request broker.
- Reintroduce deleted mechanics (memory snapshots, runtime consoles) without
  an ADR.

If a candidate version fails qualification, stay on the current pin and file
the findings upstream; the exact pin is what makes waiting safe.
