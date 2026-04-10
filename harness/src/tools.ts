import { ABoxAdapter } from "./aboxAdapter.js";
import { RiskLevel, type ToolCall, type ToolResult, type ToolSpec } from "./models.js";

type ToolFn = (call: ToolCall) => Promise<ToolResult>;

export class ToolRuntime {
  private readonly specs: Map<string, ToolSpec>;
  private readonly handlers: Map<string, ToolFn>;

  public constructor(private readonly adapter: ABoxAdapter) {
    this.specs = new Map<string, ToolSpec>([
      ["shell", { name: "shell", description: "Run read-only shell command in abox stream", risk: RiskLevel.Read }],
      [
        "shell_write",
        {
          name: "shell_write",
          description: "Run write-capable shell command",
          risk: RiskLevel.Write,
          requiresWrite: true,
        },
      ],
      ["git_status", { name: "git_status", description: "Inspect git status in stream", risk: RiskLevel.Read }],
      [
        "fetch_url",
        {
          name: "fetch_url",
          description: "Network fetch in stream",
          risk: RiskLevel.Network,
          requiresNetwork: true,
        },
      ],
    ]);

    this.handlers = new Map<string, ToolFn>([
      ["shell", (call) => this.runShell(call)],
      ["shell_write", (call) => this.runShell(call)],
      ["git_status", (call) => this.gitStatus(call)],
      ["fetch_url", (call) => this.fetchUrl(call)],
    ]);
  }

  public spec(name: string): ToolSpec | undefined {
    return this.specs.get(name);
  }

  public async execute(call: ToolCall): Promise<ToolResult> {
    const handler = this.handlers.get(call.tool);
    if (!handler) {
      return { ok: false, output: `validation_error: unknown tool: ${call.tool}`, metadata: { errorType: "validation_error" } };
    }

    try {
      return await handler(call);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return { ok: false, output: `tool_error: ${message}`, metadata: { errorType: "tool_error" } };
    }
  }

  private async runShell(call: ToolCall): Promise<ToolResult> {
    const command = String(call.args.command ?? "").trim();
    if (command.length === 0) {
      return { ok: false, output: "validation_error: missing required arg: command", metadata: { errorType: "validation_error" } };
    }
    const timeout = this.spec(call.tool)?.timeoutSeconds ?? 120;
    return this.adapter.runInStream(call.streamId, command, timeout);
  }

  private async gitStatus(call: ToolCall): Promise<ToolResult> {
    return this.adapter.runInStream(call.streamId, "git status --short --branch", 120);
  }

  private async fetchUrl(call: ToolCall): Promise<ToolResult> {
    const url = String(call.args.url ?? "").trim();
    if (url.length === 0) {
      return { ok: false, output: "validation_error: missing required arg: url", metadata: { errorType: "validation_error" } };
    }

    const command = [
      "python - <<'PY'",
      "import json, urllib.request",
      `req=urllib.request.urlopen(${JSON.stringify(url)}, timeout=15)`,
      "print(json.dumps({'status': req.status, 'bytes': len(req.read())}))",
      "PY",
    ].join("\n");

    return this.adapter.runInStream(call.streamId, command, 30);
  }
}
