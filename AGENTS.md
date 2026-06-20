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

The crate carries two generations side by side: the v0.1 bounded `Agent::run`
loop, and the v0.2 **actor service** (`Brainstem` + `Mind`) from spec 002, which
supersedes it. The v0.2 split separates **cognition** (the `Mind`: provider
calls, planning, working memory, token budget, LLM-call resilience) from
**runtime** (the `Brainstem`: inbox, peripheral/tool registry, drive loop,
cancellation, step-liveness, events).

| Path | Responsibility |
|------|----------------|
| `src/lib.rs` | re-exports; v0.1 `Agent::run` (bounded loop) |
| `src/mind/` | `Mind` trait; `Perception`/`Command`/`Decision`/`Reason`/`TaskFault`; `ModelMind` (provider + working memory + budget + classify/retry/backoff + malformed cap + throttle); `FakeMind` |
| `src/brainstem/` | `Brainstem::run` perpetual drive loop (inbox/status/cancel `select!`, pinned decide, `spawn_blocking` actuate, throttle sleep); `Task`, `Snapshot`, `Lifecycle` |
| `src/observation.rs` | `Observation`, `Outcome`, `TaskOutcome` |
| `src/budget.rs` | v0.2 renewable window: `Period`, `RenewableBudget`, `BudgetState`, `BudgetSummary` (pure, saturating); v0.1 `Budget`/`TerminalReason` |
| `src/action.rs` | v0.1 `Action`, `ActionOutcome`, `RecoverableError` |
| `src/memory.rs` | v0.1 transcript + typed observations + snapshot |
| `src/event.rs` | `RunEvent` (extended for v0.2: `TaskReceived`/`Command`/`CommandResult`/`RetryScheduled`/`WindowReset`/`ThrottleSleep`/`TaskCompleted`/`TaskFailed`/`Recovered`/`Terminated`), `Termination` |
| `src/error.rs` | `ProviderError` (`Api{status}`/`Transport`/`Decode`), `AgentError`, `ErrorClass` classification |
| `src/tool/` | `Tool` (sync), `ToolRegistry` (`Arc`-shared, `Send+Sync`) |
| `src/provider/` | `Provider` trait, OpenAI-compatible adapter, fake |
| `src/planner/` | v0.1 `Planner` trait, `RulePlanner`, `ModelPlanner` (folded into `mind/` for v0.2) |

## Conventions

- `cargo fmt` + `cargo clippy` clean before every commit.
- Unit tests in `mod tests` per module; cross-API integration tests in `tests/`.
- Conventional Commits (`feat`, `fix`, `docs`, `test`, `refactor`, `chore`).
- Tests are deterministic and offline — `FakeProvider` drives the loop, no
  network.
