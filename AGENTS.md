# AGENTS.md — agent-rs

A production-shaped Rust crate for running an LLM agent loop: perceive → plan →
act → observe. Designed around the three things a from-scratch agent loop most
often gets wrong: **recoverable errors, native LLM tool-use, and
budgets/observability.**

## Verify (single gate)

```bash
nix develop -c make check     # cargo fmt --check + clippy -D warnings + test
```

There is no other "how do I run it" tribal knowledge. `make check` is the gate;
CI and humans run the same command.

## Design principles

- **Errors are observations, not exits.** A tool failure or malformed model
  response becomes a `RecoverableError` recorded in memory; the loop continues
  and the model can correct. Only `Finish`, an exhausted budget, or a fatal
  transport error terminates a run. Never disguise a failure as a success.
- **The core loop has no tool-specific branching.** Tool calls dispatch through
  `ToolRegistry`. Adding a tool never touches the loop.
- **Typed boundaries.** Explicit error enums (`thiserror`), `serde` at the I/O
  edge. No `Box<dyn Error>` soup.
- **Simplest design that satisfies the current scope.** No speculative
  abstractions. See `docs/specs/` and `docs/plans/`.

## Module map

v0.2 **Mind + Brainstem** actor service (spec 002). Cognition and runtime are split.

| Path | Responsibility |
|------|----------------|
| `src/lib.rs` | crate re-exports |
| `src/mind/` | `Mind` trait; `Perception`/`Command`/`Decision`/`Reason`/`TaskFault`; `ModelMind` (provider + working memory + budget + classify/retry/backoff + malformed cap + throttle); `FakeMind` |
| `src/brainstem/` | `Brainstem::run` perpetual drive loop (inbox/status/cancel `select!`, pinned decide, `spawn_blocking` actuate, throttle sleep); `Task`, `Snapshot`, `Lifecycle` |
| `src/observation.rs` | `Observation`, `Outcome`, `TaskOutcome` |
| `src/recoverable.rs` | `RecoverableError` — failures the loop observes and feeds back to the model (`UnknownTool`, `ToolFailed`) |
| `src/budget.rs` | Renewable token window: `Period`, `RenewableBudget`, `BudgetState`, `BudgetSummary` |
| `src/event.rs` | `RunEvent` (`TaskReceived`/`Command`/`CommandResult`/`RecoverableObservation`/`RetryScheduled`/`WindowReset`/`ThrottleSleep`/`TaskCompleted`/`TaskFailed`/`Terminated`), `Termination` |
| `src/error.rs` | `ProviderError` (`Api{status}`/`Transport`/`Decode`), `AgentError`, `ErrorClass` classification |
| `src/tool/` | `Tool` (sync), `ToolRegistry` (`Arc`-shared, `Send+Sync`) |
| `src/provider/` | `Provider` trait, OpenAI-compatible adapter, fake |

## Conventions

- `cargo fmt` + `cargo clippy` clean before every commit.
- Unit tests in `mod tests` per module; cross-API integration tests in `tests/`.
- Conventional Commits (`feat`, `fix`, `docs`, `test`, `refactor`, `chore`).
- Tests are deterministic and offline — `FakeProvider` drives the loop, no
  network.
