1) Make the security promise literally true

This is the top priority. The explainer says the HTTPS egress proxy “does almost nothing” today and is just a TCP CONNECT tunnel, while the roadmap explicitly marks HTTPS credential injection as the P0 item because the current behavior is only “half-true” relative to the project’s promise. The good news is that the design is already there: the default policy file already models host-side header injection for Anthropic, OpenAI, Google, and GitHub, and there is a detailed implementation plan covering CA bootstrap, TLS termination, per-sandbox listeners, cert-pinning bypasses, and end-to-end tests.

What I would ship in one milestone:

Implement the TLS-terminating proxy exactly as planned.
Add CA rotation and document the CA key lifecycle clearly.
Add a bypass list for pinned clients.
Add a live e2e test against a real upstream.
Update README and explainer so the docs match reality.

Done means: guest SDK calls work with no API keys inside the VM, egress audit records are attributed to the right sandbox, and the README is no longer ahead of the implementation.

2) Put a real VM boot in CI

Right now GitHub Actions only runs phases 1–5, and the roadmap says the full VM phase is skipped on stock runners because they do not expose /dev/kvm. That means regressions in the real boot path, vsock bridge, guest init, or exit-code handoff can still land with green CI.

I would do this in two steps:

Stand up a self-hosted KVM runner and run phase 6 nightly first.
Once stable, make phase 6 required for merges to main.

Done means: every meaningful change is validated by an actual KVM-backed microVM boot, not just mocks and partial e2e coverage.

3) Close the cheap reliability gaps before adding features

The roadmap already lists a cluster of low-effort, high-value hardening work: a guard for the 108-byte Unix socket path limit, a missing test for the “no exit code → rollback + rc=1” path, a clearer stderr warning for silent guest failure, a test for the shim’s cwd fallback, a real detach integration test, and the x86_64-only bootstrap limitation. None of these are glamorous, but they are exactly the kinds of edge cases that make infra tools feel flaky.

I would batch N2–N7 into a single stabilization release and block new feature work until it is green. That is the fastest way to improve trust per unit of engineering time.

4) Make sandbox startup feel dramatically faster

This is the biggest product leap available. The roadmap says the snapshot/restore plumbing already exists conceptually, abox template create is open, and a real snapshot-based clone path could cut per-sandbox startup from seconds to under 100 ms. That is the point where abox stops feeling like “secure but a bit heavy” and starts feeling like a practical batch-agent runtime.

I would make this the next major feature after F3:

Expose abox template create.
Implement real snapshot/restore, not just CLI wiring.
Add cold-start vs warm-start benchmarks to the repo.
Make template-backed sandboxes the fast path.

Done means: the project can show a benchmarked warm-start path in the sub-100 ms range it already aspires to.

5) Replace “clone and build” with a normal installation story

Today the install path is still git clone, cargo build --release, and just bootstrap-vm. The bootstrap script downloads Cloud Hypervisor, virtiofsd, a kernel, Alpine minirootfs, and builds the shim locally, and the repo currently has no GitHub releases. That is acceptable for an experiment, but it will cap adoption hard.

I would add a release track with:

Versioned Linux binaries.
Prebuilt VM assets where possible.
Checksummed release manifests.
A one-command installer for Linux.
Release notes that state exactly what host/arch matrix is supported.

Done means: a user no longer needs to build the project from source just to try it.

6) Make Linux ARM64 first-class before promising macOS

The roadmap is explicit that bootstrap is x86_64-only today because URLs, SHAs, and the shim target are hard-coded, while Linux aarch64 looks achievable with parameterization. It is equally explicit that macOS host support is a different hypervisor problem and currently out of scope.

So I would split this into two tracks:

Ship Linux x86_64 and Linux aarch64 support now.
Write an ADR for macOS host architecture before making user-facing promises.

Done means: ARM Linux passes the same e2e path, and macOS moves from “recurring ask” to a concrete design decision.

7) Add the controls that make many-agent usage practical

The current config only gives static VM defaults for memory and vCPUs, and the roadmap’s longer-term section calls out per-sandbox cgroup v2 limits and ephemeral worktree runs as the next practical controls. Those matter more than UI polish because they turn abox into something you can safely run at scale.

I would add:

--memory
--cpus
--timeout
--ephemeral

Done means: you can run many agents in parallel without one bad run consuming the whole machine, and you can spin up “explore-only” sandboxes without creating long-lived branches.

8) Turn logs into actual telemetry

The roadmap says abox currently has three core signals—stdout, stderr/tracing, and a JSONL audit log—and proposes structured guest telemetry plus a /metrics endpoint. That is the right next step. The existing aboxstatus channel already carries exit status, so it is a natural place to add wall time, peak RSS, network bytes, and proxy call counts.

I would ship:

Structured run summary JSON.
Prometheus /metrics.
abox inspect <task> for post-run diagnostics.
Basic audit log filtering by sandbox/task.

Done means: a user can answer “what did this agent do, what did it cost, and why did it fail?” without reading raw logs.

9) Build credibility around security, not just features

abox’s differentiator is the trust boundary: policy-governed host execution, per-VM provenance, and credentials that are supposed to stay on the host. The repo already documents that architecture clearly. That makes a security credibility track worth real investment.

I would add:

A first-class threat model doc.
Fuzzing for proxy request parsing and policy evaluation.
Signed release artifacts and SBOMs.
A focused third-party review of the proxy/policy path.

Done means: the security story is not just plausible; it is auditable.