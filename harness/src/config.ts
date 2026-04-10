import { readFile } from "node:fs/promises";

import { Mode, type AutonomyBudget } from "./models.js";
import { defaultHarnessConfig, type HarnessConfig } from "./orchestrator.js";
import type { PolicyConfig } from "./policy.js";

export type RuntimeFileConfig = {
  runtime?: Partial<{
    mode: Mode;
    maxParallelStreams: number;
    autoEscalate: boolean;
    assumeDangerousSkipPermissions: boolean;
    checkpointEveryNSteps: number;
  }>;
  policy?: Partial<{
    allowedTools: string[];
    writeTools: string[];
    networkTools: string[];
    destructiveTools: string[];
    requireEscalationForWrite: boolean;
    requireEscalationForNetwork: boolean;
  }>;
  budget?: Partial<AutonomyBudget>;
};

export const loadConfig = async (path: string): Promise<RuntimeFileConfig> => {
  const raw = await readFile(path, "utf8");
  return JSON.parse(raw) as RuntimeFileConfig;
};

export const buildRuntimeConfig = (fileConfig: RuntimeFileConfig): HarnessConfig => {
  const defaults = defaultHarnessConfig();
  return {
    mode: fileConfig.runtime?.mode ?? defaults.mode,
    maxParallelStreams: fileConfig.runtime?.maxParallelStreams ?? defaults.maxParallelStreams,
    autoEscalate: fileConfig.runtime?.autoEscalate ?? defaults.autoEscalate,
    assumeDangerousSkipPermissions:
      fileConfig.runtime?.assumeDangerousSkipPermissions ?? defaults.assumeDangerousSkipPermissions,
    checkpointEveryNSteps: fileConfig.runtime?.checkpointEveryNSteps ?? defaults.checkpointEveryNSteps,
    budget: {
      maxTotalSteps: fileConfig.budget?.maxTotalSteps ?? defaults.budget.maxTotalSteps,
      maxWriteOps: fileConfig.budget?.maxWriteOps ?? defaults.budget.maxWriteOps,
      maxNetworkOps: fileConfig.budget?.maxNetworkOps ?? defaults.budget.maxNetworkOps,
      maxDestructiveOps: fileConfig.budget?.maxDestructiveOps ?? defaults.budget.maxDestructiveOps,
    },
  };
};

export const buildPolicyConfig = (fileConfig: RuntimeFileConfig, mode: Mode, assumeDangerous: boolean): PolicyConfig => ({
  mode,
  allowedTools: new Set(fileConfig.policy?.allowedTools ?? ["shell", "shell_write", "git_status", "fetch_url"]),
  writeTools: new Set(fileConfig.policy?.writeTools ?? ["shell_write"]),
  networkTools: new Set(fileConfig.policy?.networkTools ?? ["fetch_url"]),
  destructiveTools: new Set(fileConfig.policy?.destructiveTools ?? []),
  requireEscalationForWrite: fileConfig.policy?.requireEscalationForWrite ?? false,
  requireEscalationForNetwork: fileConfig.policy?.requireEscalationForNetwork ?? false,
  assumeDangerousSkipPermissions: assumeDangerous,
});
