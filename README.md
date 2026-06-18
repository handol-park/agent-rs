# agent-rs

A small, production-shaped Rust crate for running an LLM agent loop:
**perceive → plan → act → observe**, with recoverable errors, native LLM
tool-use, and budgets/observability built in from the start.

It is the "done right" successor to the [`agy`](../agy) learning project. The
defining difference: a tool failure or malformed model response is recorded as a
`RecoverableError` and fed back into the loop so the model can correct — it is
never silently turned into a successful finish.

## Quick start

The Rust toolchain comes from the Nix dev shell:

```bash
nix develop -c make check                 # fmt --check + clippy -D warnings + test
nix develop -c cargo run --example run    # one agent turn (RulePlanner with no API key)
```

Point it at a real provider via env:

```bash
export LLM_BASE_URL=https://api.openai.com/v1
export LLM_API_KEY=sk-...
export LLM_MODEL=gpt-4o-mini
nix develop -c cargo run --example run
```

## Design

See `AGENTS.md` for principles and the module map, `docs/specs/001-agent-core.md`
for the contract, and `docs/plans/001-agent-core.md` for the implementation plan.

## Status

v0.1 — core loop, OpenAI-compatible provider, tool registry, budgets, event
stream. Out of scope for now: streaming, multi-agent, MCP, persistence beyond an
in-memory snapshot.
