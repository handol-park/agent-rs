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

The crate carries two generations. **v0.2 (the actor service, spec 002)
supersedes v0.1's single `Agent::run` loop** and is the path to build on; v0.1
is kept for reference and the `examples/run.rs` demo.

**v0.2 — Mind + Brainstem.** Cognition and runtime are split:

- **Mind** (`src/mind/`) is cognition. `Mind::decide(Perception) -> Decision`
  owns the `Provider`, working memory, the renewable token budget, and
  LLM-call resilience: `ModelMind` classifies `ProviderError`s
  (transient/service-fatal/task-fatal), retries transient calls with capped
  exponential backoff + jitter (unbounded), caps malformed re-prompts at 2, and
  throttles when the token window is exhausted. `FakeMind` scripts decisions for
  brainstem tests.
- **Brainstem** (`src/brainstem/`) is the runtime/body. `Brainstem::run` is a
  perpetual drive loop: an idle `select!` over `{cancel, status, inbox}` pulls a
  `Task`, then per turn refreshes a cached `Snapshot`, pins `mind.decide`, and
  drives it — actuating `Command`s off-loop via `spawn_blocking` over the
  `Arc<ToolRegistry>`, sleeping on `Throttle`, answering `Status` mid-flight, and
  honoring cancellation. Errors are recoverable `Observation`s
  (`src/observation.rs`); only a service-fatal `Reason`, cancellation, or inbox
  close terminates (`Termination`). Step-liveness (`max_steps`) bounds
  non-convergence as a task-fatal `NoProgress`.

Supporting modules: `src/budget.rs` (renewable window — `Period`,
`RenewableBudget`, pure saturating `BudgetState`), `src/error.rs`
(`ProviderError::Api{status}` + `ErrorClass` classification), and `RunEvent`
extensions in `src/event.rs` (`TaskReceived`/`Command`/`CommandResult`/
`RetryScheduled`/`WindowReset`/`ThrottleSleep`/`TaskCompleted`/`TaskFailed`/
`Recovered`/`Terminated`). See `examples/service.rs` for end-to-end wiring.

**v0.1 — `Agent::run`** (`src/lib.rs`): a bounded loop. Each step checks budgets
→ builds a `PlanContext` → `Planner::plan_next` (wall-clock `timeout`) → executes
each `Action` via `ToolRegistry` → records `ActionOutcome`s into `Memory` → emits
`RunEvent`s. Terminates on `Finish`, budget exhaustion, timeout, or fatal error.

Runtime dispatch: `Provider`, `Planner`, and `Mind` are `#[async_trait]` trait
objects (`Box<dyn>`), selected from env at startup. `Tool` is sync. See
`AGENTS.md` for the module map and `docs/` for spec + plan.

## Non-negotiables

- Spec (`docs/specs/`) before plan (`docs/plans/`) before code.
- Every change passes `make check`.
- No error is silently turned into a successful `Finish`.
