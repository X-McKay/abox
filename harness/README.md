# ABox Agent Harness (TypeScript)

A lightweight, robust custom agent harness designed around `abox` sandboxing and high-autonomy operation in `dangerously-skip-permissions`-style environments.

## Why TypeScript

TypeScript aligns better with modern agent ecosystems (Claude/Codex-style tooling, MCP-first integrations, server/CLI UX), while still allowing a lightweight core.

## Core design

- **Planner → Executor contract**: explicit step contracts with dependencies and acceptance metadata.
- **Mode-aware policy**: `plan`, `build`, `review` behaviors via tool allowlists.
- **Dangerous-mode guardrails**: deterministic policy + autonomy budgets instead of interactive prompts.
- **Parallel workstreams**: bounded stream concurrency over `abox run --task-id ...`.
- **Durable traces**: step traces and periodic checkpoint summaries.

## Project layout

- `src/models.ts`: shared domain types (`Mode`, `RiskLevel`, `PlanStep`, budgets).
- `src/policy.ts`: deterministic policy evaluation.
- `src/tools.ts`: tool registry and normalized error handling.
- `src/aboxAdapter.ts`: stream-scoped command execution via `abox`.
- `src/orchestrator.ts`: planner/coordinator/executor loop.
- `src/config.ts`: JSON config loading and config builders.
- `src/cli.ts`: non-interactive command runner.
- `config/default.json`: default runtime/policy/budget profile.
- `tests/harness.test.ts`: policy/orchestration budget tests.

## Usage

```bash
cd harness
npm run test
npm run start -- --goal "echo hello" --streams s1,s2
```

Or directly:

```bash
node dist/src/cli.js --goal "echo hello" --config harness/config/default.json --streams s1,s2
```

## Notes

- Uses Node built-ins only (no runtime dependencies).
- Entire harness remains isolated under `harness/` for future extraction.
- Designed to add MCP providers and headless server mode in future iterations.
