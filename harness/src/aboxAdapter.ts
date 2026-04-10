import { execFile } from "node:child_process";
import { promisify } from "node:util";

import type { ToolResult } from "./models.js";

const execFileAsync = promisify(execFile);

export class ABoxAdapter {
  public constructor(private readonly aboxBin: string = "abox") {}

  public async runInStream(streamId: string, command: string, timeoutSeconds = 120): Promise<ToolResult> {
    const cmd = ["run", "--task-id", streamId, "--", "bash", "-lc", command];
    try {
      const { stdout, stderr } = await execFileAsync(this.aboxBin, cmd, {
        timeout: timeoutSeconds * 1000,
        windowsHide: true,
      });
      const output = [stdout, stderr].filter(Boolean).join("\n").trim();
      return { ok: true, output, metadata: { errorType: "ok", cmd: [this.aboxBin, ...cmd] } };
    } catch (error) {
      const err = error as { code?: string | number; stdout?: string; stderr?: string; signal?: string; message?: string };
      const output = [err.stdout ?? "", err.stderr ?? "", err.message ?? ""].filter(Boolean).join("\n").trim();
      const timeout = err.code === "ETIMEDOUT";
      return {
        ok: false,
        output: timeout ? `timeout: ${output}` : output,
        metadata: {
          errorType: timeout ? "timeout" : "nonzero_exit",
          code: String(err.code ?? "unknown"),
          signal: err.signal ?? "",
          cmd: [this.aboxBin, ...cmd],
        },
      };
    }
  }
}
