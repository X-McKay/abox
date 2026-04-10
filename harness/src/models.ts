export enum Decision {
  Allow = "allow",
  Deny = "deny",
  Escalate = "escalate",
}

export enum Mode {
  Plan = "plan",
  Build = "build",
  Review = "review",
}

export enum RiskLevel {
  Read = "read",
  Write = "write",
  Network = "network",
  Destructive = "destructive",
}

export type ToolSpec = {
  name: string;
  description: string;
  risk: RiskLevel;
  requiresWrite?: boolean;
  requiresNetwork?: boolean;
  destructive?: boolean;
  timeoutSeconds?: number;
};

export type ToolCall = {
  tool: string;
  args: Record<string, unknown>;
  streamId: string;
};

export type ToolResult = {
  ok: boolean;
  output: string;
  metadata?: Record<string, unknown>;
};

export type PolicyResult = {
  decision: Decision;
  reason: string;
};

export type PlanStep = {
  id: string;
  streamId: string;
  action: string;
  args: Record<string, unknown>;
  dependsOn: string[];
  acceptanceCheck?: string;
  rollbackHint?: string;
  risk: RiskLevel;
};

export type TurnTrace = {
  stepId: string;
  streamId: string;
  tool: string;
  decision: Decision;
  ok: boolean;
  detail: string;
};

export type SessionMemory = {
  goal: string;
  streamNotes: Record<string, string[]>;
  durableSummary: string[];
};

export type AutonomyBudget = {
  maxTotalSteps: number;
  maxWriteOps: number;
  maxNetworkOps: number;
  maxDestructiveOps: number;
};

export type BudgetState = {
  totalSteps: number;
  writeOps: number;
  networkOps: number;
  destructiveOps: number;
};

export const defaultBudget = (): AutonomyBudget => ({
  maxTotalSteps: 60,
  maxWriteOps: 20,
  maxNetworkOps: 10,
  maxDestructiveOps: 0,
});
