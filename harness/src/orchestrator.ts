import {
  Decision,
  Mode,
  RiskLevel,
  defaultBudget,
  type AutonomyBudget,
  type PlanStep,
  type SessionMemory,
  type ToolCall,
} from "./models.js";
import { MemoryStore } from "./memory.js";
import { PolicyEngine, type PolicyConfig } from "./policy.js";
import { ToolRuntime } from "./tools.js";

export type HarnessConfig = {
  mode: Mode;
  maxParallelStreams: number;
  autoEscalate: boolean;
  assumeDangerousSkipPermissions: boolean;
  checkpointEveryNSteps: number;
  budget: AutonomyBudget;
};

export const defaultHarnessConfig = (): HarnessConfig => ({
  mode: Mode.Build,
  maxParallelStreams: 3,
  autoEscalate: true,
  assumeDangerousSkipPermissions: true,
  checkpointEveryNSteps: 4,
  budget: defaultBudget(),
});

export class AgentHarness {
  public constructor(
    private readonly runtime: ToolRuntime,
    private readonly policy: PolicyEngine,
    private readonly config: HarnessConfig,
  ) {}

  public async executeGoal(goal: string, streams: string[]): Promise<MemoryStore> {
    const state: SessionMemory = {
      goal,
      streamNotes: {},
      durableSummary: [],
    };
    const memory = new MemoryStore(state);
    const steps = this.plan(goal, streams);
    const done = new Set<string>();

    while (done.size < steps.length) {
      const ready = steps.filter((step) => !done.has(step.id) && step.dependsOn.every((dep) => done.has(dep)));
      if (ready.length === 0) {
        memory.checkpoint("blocked_by_dependencies");
        break;
      }

      const queue = [...ready];
      const workers = Array.from({ length: Math.min(this.config.maxParallelStreams, queue.length) }, async () => {
        while (queue.length > 0) {
          const next = queue.shift();
          if (!next) {
            return;
          }
          await this.executeStep(next, memory);
          done.add(next.id);
        }
      });

      await Promise.all(workers);
    }

    memory.checkpoint("goal_complete");
    return memory;
  }

  private async executeStep(step: PlanStep, memory: MemoryStore): Promise<void> {
    if (!this.consumeBudget(step.risk, memory)) {
      memory.logTrace({
        stepId: step.id,
        streamId: step.streamId,
        tool: step.action,
        decision: Decision.Deny,
        ok: false,
        detail: "autonomy budget exceeded",
      });
      return;
    }

    const spec = this.runtime.spec(step.action);
    if (!spec) {
      memory.logTrace({
        stepId: step.id,
        streamId: step.streamId,
        tool: step.action,
        decision: Decision.Deny,
        ok: false,
        detail: "unknown tool requested",
      });
      return;
    }

    const decision = this.policy.evaluate(spec);
    if (decision.decision === Decision.Deny) {
      memory.logTrace({
        stepId: step.id,
        streamId: step.streamId,
        tool: step.action,
        decision: decision.decision,
        ok: false,
        detail: decision.reason,
      });
      return;
    }

    if (decision.decision === Decision.Escalate && !this.config.autoEscalate) {
      memory.logTrace({
        stepId: step.id,
        streamId: step.streamId,
        tool: step.action,
        decision: decision.decision,
        ok: false,
        detail: decision.reason,
      });
      return;
    }

    const call: ToolCall = { tool: step.action, args: step.args, streamId: step.streamId };
    const result = await this.runtime.execute(call);
    memory.logTrace({
      stepId: step.id,
      streamId: step.streamId,
      tool: step.action,
      decision: decision.decision,
      ok: result.ok,
      detail: result.ok ? result.output : `failed: ${result.output}`,
    });

    if (memory.budgetState.totalSteps % this.config.checkpointEveryNSteps === 0) {
      memory.checkpoint(`step_${memory.budgetState.totalSteps}`);
    }
  }

  private consumeBudget(risk: RiskLevel, memory: MemoryStore): boolean {
    const state = memory.budgetState;
    const budget = this.config.budget;
    if (state.totalSteps >= budget.maxTotalSteps) {
      return false;
    }

    state.totalSteps += 1;
    if (risk === RiskLevel.Write) {
      state.writeOps += 1;
      return state.writeOps <= budget.maxWriteOps;
    }
    if (risk === RiskLevel.Network) {
      state.networkOps += 1;
      return state.networkOps <= budget.maxNetworkOps;
    }
    if (risk === RiskLevel.Destructive) {
      state.destructiveOps += 1;
      return state.destructiveOps <= budget.maxDestructiveOps;
    }
    return true;
  }

  private plan(goal: string, streams: string[]): PlanStep[] {
    return streams.flatMap((stream, index) => {
      const statusId = `${index}-status`;
      const goalId = `${index}-goal`;
      const isBuild = this.config.mode === Mode.Build;
      return [
        {
          id: statusId,
          streamId: stream,
          action: "git_status",
          args: {},
          dependsOn: [],
          acceptanceCheck: "exit_code=0",
          risk: RiskLevel.Read,
        },
        {
          id: goalId,
          streamId: stream,
          action: isBuild ? "shell_write" : "shell",
          args: { command: goal },
          dependsOn: [statusId],
          acceptanceCheck: "exit_code=0",
          rollbackHint: "git checkout -- .",
          risk: isBuild ? RiskLevel.Write : RiskLevel.Read,
        },
      ];
    });
  }
}

export const buildPolicy = (config: PolicyConfig): PolicyEngine => new PolicyEngine(config);
