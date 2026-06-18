# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

`agent-rs` is a production-shaped Rust agent framework: a bounded perceive →
plan → act → observe loop over a pluggable `Provider` (LLM) and a typed
`ToolRegistry`. It is the "done right" successor to the `agy` learning project —
the key improvement is that **errors are recoverable observations, not terminal
states**.

## Build & verify

The Rust toolchain is provided by the Nix dev shell — `cargo`/`rustc` are not on
the bare PATH. Always run cargo through it:

```bash
nix develop -c make check          # the gate: fmt --check + clippy -D warnings + test
nix develop -c cargo check         # fast compile check
nix develop -c cargo test <name>   # single test
```

## Architecture

`Agent::run` (`src/lib.rs`) drives the loop. Each step: check budgets → build a
`PlanContext` → `Planner::plan_next` (wrapped in a wall-clock `timeout`) →
execute each `Action` via `ToolRegistry` → record `ActionOutcome`s into
`Memory` → emit `RunEvent`s. Terminates only on `Finish`, budget exhaustion, a
timeout, or a fatal error.

Runtime dispatch: `Provider` and `Planner` are `#[async_trait]` trait objects
(`Box<dyn>`), selected from env at startup. `Tool` is sync. See `AGENTS.md` for
the module map and `docs/` for spec + plan.

## Non-negotiables

- Spec (`docs/specs/`) before plan (`docs/plans/`) before code.
- Every change passes `make check`.
- No error is silently turned into a successful `Finish`.
