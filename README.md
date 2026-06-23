# agent-rs

A small, production-shaped Rust crate for running an LLM agent as a service:
**Mind + Brainstem** — cognition and runtime are split. The `Brainstem` drives a
perpetual task loop; `Mind` owns the LLM provider and resilience logic.

It is the "done right" successor to the [`agy`](../agy) learning project. The
defining difference: errors are recoverable observations, not terminal states — a
tool failure is fed back to the model as a `RecoverableError` so it can correct,
and a malformed model response gets up to two re-prompts before failing the task.
Nothing is silently turned into a successful finish.

## Quick start

The Rust toolchain comes from the Nix dev shell:

```bash
nix develop -c make check                     # fmt --check + clippy -D warnings + test
nix develop -c cargo run --example service    # end-to-end actor service wiring
```

Point it at a real provider via env:

```bash
export LLM_BASE_URL=https://api.openai.com/v1
export LLM_API_KEY=sk-...
export LLM_MODEL=gpt-4o-mini
nix develop -c cargo run --example service
```

## Design

See `AGENTS.md` for principles and the module map, `docs/specs/002-resilient-loop-renewable-budget.md`
for the contract, and `docs/plans/002-resilient-loop-renewable-budget.md` for the implementation plan.

## Status

v0.2 — Mind + Brainstem actor service. Features: OpenAI-compatible provider,
tool registry, renewable token budgets, event stream, transient-error retry with
backoff, throttle-on-exhaustion. Out of scope for now: streaming, multi-agent,
MCP, persistence.
