# Plan 001 — agent-rs core (v0.1)

Implementation plan for [Spec 001](../specs/001-agent-core.md). _(informational
unless quoting a spec MUST)_

## Dependencies

`tokio` (rt-multi-thread, macros, time, sync), `async-trait`, `reqwest` (json,
rustls-tls), `serde`/`serde_json`, `thiserror`.

## Module map

```
src/lib.rs            Agent, the run loop, re-exports
src/error.rs          AgentError / ProviderError / ToolError / PlannerError
src/action.rs         Action, ActionOutcome, RecoverableError
src/memory.rs         Memory, Observation, MemorySnapshot
src/budget.rs         Budget, BudgetState, TerminalReason
src/event.rs          RunEvent
src/tool/mod.rs       Tool (sync), ToolRegistry, ToolSchema
src/tool/builtins.rs  CalculatorTool
src/provider/mod.rs   Provider, ModelRequest/Response, ToolCall, Usage, Message
src/provider/openai.rs  OpenAiProvider (chat/completions + native tool-calling)
src/provider/fake.rs    FakeProvider (scripted, deterministic)
src/planner/mod.rs    Planner, PlanContext, PlanOutput
src/planner/rule.rs   RulePlanner (offline fallback)
src/planner/model.rs  ModelPlanner (owns Box<dyn Provider>; tool_calls -> Action)
examples/run.rs       real provider via env; RulePlanner fallback
tests/loop_recovery.rs  integration: error -> observed -> corrected -> finish
```

## Core contracts

- `Action`: `CallTool { call_id, name, input }` | `Finish { message }`.
- `ActionOutcome`: `ToolResult` | `Recoverable(RecoverableError)` | `Finished`.
- `RecoverableError`: `UnknownTool` | `ToolFailed` | `MalformedPlan`.
- `Provider::complete(&ModelRequest) -> Result<ModelResponse, ProviderError>`.
- `Planner::plan_next(&PlanContext) -> Result<PlanOutput, PlannerError>` where
  `PlanOutput { thought, actions: Vec<Action> }`.
- `Budget { max_steps, max_tokens, wall_clock }`;
  `TerminalReason { Finished | MaxSteps | TokenBudget | TimedOut | Fatal }`.

## The loop (`Agent::run`)

Per step: budget check → build `PlanContext` → `timeout(remaining,
planner.plan_next)` → classify planner error (fatal vs recoverable) → execute
each `Action` (`Finish` returns; `CallTool` → registry → `ToolResult` or
`Recoverable`) → record outcomes in `Memory`, accumulate `usage`, emit
`RunEvent`s → loop. Memory holds the full transcript; each turn rebuilds the
`ModelRequest` (stateless), rendering recoverable errors back so the model sees
its mistake.

## Build order (commit per phase)

- P0 scaffold (this commit): Cargo, flake, .gitignore, AGENTS/CLAUDE/README,
  Makefile, docs, minimal lib.
- P1 pure types + unit tests: error, action, memory, budget, event.
- P2 tool/ + CalculatorTool + tests.
- P3 provider/: trait + types + FakeProvider, then OpenAiProvider.
- P4 planner/: RulePlanner, then ModelPlanner.
- P5 lib.rs loop: budgets + events + recoverable wiring.
- P6 tests/loop_recovery.rs + examples/run.rs.
- P7 make check green.

## Decisions

- Runtime dispatch for `Provider`/`Planner` (`Box<dyn>`, `#[async_trait]`),
  justified by env-driven provider selection. `Tool` is sync.
- No `AskUser` action / no `Environment` trait — human input, if ever needed, is
  a `Tool`.
- `PlanOutput` carries `Vec<Action>` (native tool-calling can return several),
  but the loop executes them sequentially in v0.1.
