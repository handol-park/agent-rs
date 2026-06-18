# Spec 001 — agent-rs core (v0.1)

Status: accepted. Normative statements use RFC 2119 keywords; everything else is
_(informational)_.

## What it is _(informational)_

A reusable Rust crate for running an LLM agent loop — perceive → plan → act →
observe — designed around the three things a from-scratch loop most often gets
wrong: recoverable errors, native LLM tool-use, and observability/budgets.

## Goals — the crate MUST…

1. **MUST** run a bounded agent loop over a `Planner`, dispatching `Tool` calls
   through a typed `ToolRegistry` with no tool-specific branching in the core
   loop.
2. **MUST** treat tool failures and malformed model output as **recoverable
   observations** fed back into memory; the loop continues and the model may
   retry. Only an explicit `Finish`, an exhausted budget, or a fatal transport
   error terminates a run. No error is silently disguised as a success.
3. **MUST** enforce three budgets: `max_steps`, a token budget, and a wall-clock
   timeout. Hitting any ends the run with a typed terminal reason.
4. **MUST** emit a structured `RunEvent` stream (step start, plan, tool call,
   tool result, recoverable error, finish) so a run is fully inspectable.
5. **MUST** ship a real provider adapter speaking the OpenAI-compatible
   `/chat/completions` API with native tool-calling (`tools` / `tool_calls`),
   configurable via `LLM_BASE_URL`, `LLM_API_KEY`, `LLM_MODEL`.
6. **MUST** be fully testable offline: a `FakeProvider` drives the entire loop
   deterministically, including a tool call, a recovered error, and a finish.
   No network in tests.
7. **MUST** pass `cargo fmt --check`, `cargo clippy -D warnings`, and
   `cargo test` via a single `make check` gate.

## Out of scope for v0.1 — MUST NOT block v1

Streaming responses, multi-agent/sub-agents, MCP, persistence beyond an
in-memory snapshot, transport retries/backoff, a CLI beyond one example binary,
and parallel tool dispatch. Human-in-the-loop input, if added later, **SHOULD**
be modeled as an ordinary `Tool`, not a privileged `Action`.

## Success criteria

- `make check` green.
- One integration test proves the full loop: model calls a tool → the tool
  errors once → the error is observed → the model corrects → `Finish`. The test
  asserts both the `Finished` outcome and that a `RecoverableError` event was
  emitted.
- One `examples/` binary runs a real agent turn when env vars are set, and falls
  back to the `RulePlanner` when they are not.

## Stack _(informational)_

Rust 2021, `tokio` + `async-trait`, `reqwest`, `serde`/`serde_json`,
`thiserror`. Async throughout. Provider and Planner use runtime dispatch
(`Box<dyn>`); `Tool` is sync.
