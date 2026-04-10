import test from "node:test";
import assert from "node:assert/strict";

import { Mode, RiskLevel, type ToolResult } from "../src/models.js";
import { AgentHarness, buildPolicy, type HarnessConfig } from "../src/orchestrator.js";
import type { PolicyConfig } from "../src/policy.js";
import { ToolRuntime } from "../src/tools.js";

class FakeAdapter {
  public async runInStream(streamId: string, command: string): Promise<ToolResult> {
    return { ok: true, output: `[${streamId}] ${command}`, metadata: { errorType: "ok" } };
  }
}

const policyConfig = (overrides: Partial<PolicyConfig> = {}): PolicyConfig => ({
  mode: Mode.Build,
  allowedTools: new Set(["shell", "shell_write", "git_status", "fetch_url"]),
  writeTools: new Set(["shell_write"]),
  networkTools: new Set(["fetch_url"]),
  destructiveTools: new Set<string>(),
  assumeDangerousSkipPermissions: true,
  requireEscalationForWrite: false,
  requireEscalationForNetwork: false,
  ...overrides,
});

const harnessConfig = (overrides: Partial<HarnessConfig> = {}): HarnessConfig => ({
  mode: Mode.Build,
  maxParallelStreams: 2,
  autoEscalate: true,
  assumeDangerousSkipPermissions: true,
  checkpointEveryNSteps: 4,
  budget: {
    maxTotalSteps: 10,
    maxWriteOps: 3,
    maxNetworkOps: 3,
    maxDestructiveOps: 0,
  },
  ...overrides,
});

test("dangerous-mode permits write without escalation", () => {
  const policy = buildPolicy(policyConfig({ requireEscalationForWrite: true }));
  const result = policy.evaluate({
    name: "shell_write",
    description: "write",
    risk: RiskLevel.Write,
    requiresWrite: true,
  });

  assert.equal(result.decision, "allow");
});

test("parallel streams execute status+goal steps", async () => {
  const runtime = new ToolRuntime(new FakeAdapter() as never);
  const policy = buildPolicy(policyConfig({ mode: Mode.Plan }));
  const harness = new AgentHarness(runtime, policy, harnessConfig({ mode: Mode.Plan }));

  const memory = await harness.executeGoal("echo hello", ["s1", "s2"]);

  assert.equal(memory.state.streamNotes.s1?.length, 2);
  assert.equal(memory.state.streamNotes.s2?.length, 2);
});

test("write budget denies excess operations", async () => {
  const runtime = new ToolRuntime(new FakeAdapter() as never);
  const policy = buildPolicy(policyConfig());
  const harness = new AgentHarness(
    runtime,
    policy,
    harnessConfig({ budget: { maxTotalSteps: 10, maxWriteOps: 1, maxNetworkOps: 2, maxDestructiveOps: 0 } }),
  );

  const memory = await harness.executeGoal("echo write", ["s1", "s2"]);
  const notes = Object.values(memory.state.streamNotes).flat().join("\n");
  assert.match(notes, /autonomy budget exceeded/);
});
