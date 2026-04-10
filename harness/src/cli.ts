import { ABoxAdapter } from "./aboxAdapter.js";
import { loadConfig, buildPolicyConfig, buildRuntimeConfig } from "./config.js";
import { AgentHarness, buildPolicy } from "./orchestrator.js";
import { ToolRuntime } from "./tools.js";

type CliArgs = {
  goal: string;
  config: string;
  streams: string[];
  aboxBin: string;
};

const parseArgs = (argv: string[]): CliArgs => {
  const result: CliArgs = {
    goal: "",
    config: "harness/config/default.json",
    streams: ["default"],
    aboxBin: "abox",
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--goal") {
      result.goal = argv[i + 1] ?? "";
      i += 1;
    } else if (arg === "--config") {
      result.config = argv[i + 1] ?? result.config;
      i += 1;
    } else if (arg === "--streams") {
      const next = argv[i + 1] ?? "";
      result.streams = next.split(",").filter(Boolean);
      i += 1;
    } else if (arg === "--abox-bin") {
      result.aboxBin = argv[i + 1] ?? result.aboxBin;
      i += 1;
    }
  }

  if (!result.goal) {
    throw new Error("missing required argument --goal");
  }

  return result;
};

export const runCli = async (argv: string[]): Promise<number> => {
  const args = parseArgs(argv);
  const fileConfig = await loadConfig(args.config);
  const runtimeConfig = buildRuntimeConfig(fileConfig);

  const runtime = new ToolRuntime(new ABoxAdapter(args.aboxBin));
  const policy = buildPolicy(
    buildPolicyConfig(fileConfig, runtimeConfig.mode, runtimeConfig.assumeDangerousSkipPermissions),
  );
  const harness = new AgentHarness(runtime, policy, runtimeConfig);

  const memory = await harness.executeGoal(args.goal, args.streams);
  process.stdout.write(memory.summarize() + "\n");
  return 0;
};

if (import.meta.url === `file://${process.argv[1]}`) {
  runCli(process.argv.slice(2))
    .then((code) => {
      process.exitCode = code;
    })
    .catch((error) => {
      const message = error instanceof Error ? error.message : String(error);
      process.stderr.write(`harness_error: ${message}\n`);
      process.exitCode = 1;
    });
}
