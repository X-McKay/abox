import { Decision, Mode, type PolicyResult, type ToolSpec } from "./models.js";

export type PolicyConfig = {
  mode: Mode;
  allowedTools: Set<string>;
  writeTools: Set<string>;
  networkTools: Set<string>;
  destructiveTools: Set<string>;
  assumeDangerousSkipPermissions: boolean;
  requireEscalationForWrite: boolean;
  requireEscalationForNetwork: boolean;
};

export class PolicyEngine {
  public constructor(private readonly config: PolicyConfig) {}

  public evaluate(tool: ToolSpec): PolicyResult {
    if (!this.config.allowedTools.has(tool.name)) {
      return { decision: Decision.Deny, reason: `tool '${tool.name}' is not allowed in mode=${this.config.mode}` };
    }

    if (tool.destructive && !this.config.destructiveTools.has(tool.name)) {
      return { decision: Decision.Deny, reason: `tool '${tool.name}' has no destructive permit` };
    }

    if (tool.requiresWrite && !this.config.writeTools.has(tool.name)) {
      return { decision: Decision.Deny, reason: `tool '${tool.name}' has no write permit` };
    }

    if (tool.requiresNetwork && !this.config.networkTools.has(tool.name)) {
      return { decision: Decision.Deny, reason: `tool '${tool.name}' has no network permit` };
    }

    if (this.config.assumeDangerousSkipPermissions) {
      return { decision: Decision.Allow, reason: "dangerous-skip-permissions profile enabled" };
    }

    if (tool.requiresWrite && this.config.requireEscalationForWrite) {
      return { decision: Decision.Escalate, reason: "write action requires escalation" };
    }

    if (tool.requiresNetwork && this.config.requireEscalationForNetwork) {
      return { decision: Decision.Escalate, reason: "network action requires escalation" };
    }

    return { decision: Decision.Allow, reason: "policy check passed" };
  }
}
