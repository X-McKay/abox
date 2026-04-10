import { type BudgetState, type SessionMemory, type TurnTrace } from "./models.js";

export class MemoryStore {
  public readonly budgetState: BudgetState = {
    totalSteps: 0,
    writeOps: 0,
    networkOps: 0,
    destructiveOps: 0,
  };

  public constructor(public readonly state: SessionMemory) {}

  public logTrace(trace: TurnTrace): void {
    const note = `step=${trace.stepId} tool=${trace.tool} decision=${trace.decision} ok=${trace.ok} detail=${trace.detail.slice(0, 240)}`;
    const notes = this.state.streamNotes[trace.streamId] ?? [];
    notes.push(note);
    this.state.streamNotes[trace.streamId] = notes;
  }

  public checkpoint(reason: string): void {
    const streamCounts = Object.entries(this.state.streamNotes)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([stream, notes]) => `${stream}:${notes.length}`)
      .join(", ");
    this.state.durableSummary.push(`checkpoint=${reason} streams=[${streamCounts}]`);
  }

  public summarize(): string {
    const parts: string[] = ["Goal: " + this.state.goal];
    for (const [streamId, notes] of Object.entries(this.state.streamNotes).sort(([a], [b]) => a.localeCompare(b))) {
      parts.push(`[${streamId}] ${notes.length} events`);
      if (notes.length > 0) {
        parts.push(`  last: ${notes.at(-1)}`);
      }
    }
    if (this.state.durableSummary.length > 0) {
      parts.push("Durable summary:");
      parts.push(...this.state.durableSummary.slice(-5).map((line) => `  - ${line}`));
    }
    return parts.join("\n");
  }
}
