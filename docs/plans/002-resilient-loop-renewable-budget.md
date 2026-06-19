# Plan 002 — actor agent: mind + brainstem (v0.2)

Implementation plan for [Spec 002](../specs/002-resilient-loop-renewable-budget.md).
_(informational unless quoting a spec MUST. Goal numbers below refer to that spec.)_

## Dependencies

Over [Plan 001](001-agent-core.md): add `tokio-util` (`CancellationToken`) and
the `tokio` `rt-multi-thread` feature (`spawn_blocking`). Backoff + jitter are
in-crate (a tiny seedable xorshift PRNG — no `rand` dependency); the seed is
injectable so tests are deterministic. **No date library** — time is
`tokio::time::Instant`, pausable under `tokio::test(start_paused)` (goal 18).

## Module map

```
src/lib.rs              re-exports; Brainstem entry; Termination
src/mind/mod.rs         Mind trait; Perception, Command, Decision, Outcome, Reason, TaskFault
src/mind/model.rs       ModelMind: owns Box<dyn Provider> + working memory + Budget; classify/retry/backoff; malformed cap; throttle
src/mind/fake.rs        FakeMind (scripted Decisions) for brainstem tests
src/brainstem/mod.rs    Brainstem, run loop (select!/pin/spawn_blocking), Snapshot, Lifecycle, BrainstemState, Termination
src/observation.rs      Observation, Outcome, TaskOutcome
src/budget.rs           Period, Budget, BudgetState (renewable window; pure fns; saturating)
src/event.rs            RunEvent (extended for v0.2 paths)
src/error.rs            ProviderError { Transport, Api{status,body}, Decode }, AgentError, classification
src/tool/…              from 001: Tool (sync), ToolRegistry (now Arc + Send+Sync), builtins
src/provider/…          from 001: Provider, OpenAiProvider, FakeProvider (Api now carries HTTP status)
examples/service.rs     perpetual service: spawn Brainstem, feed Tasks via inbox, cancel
tests/actor_loop.rs     integration: the success-criteria scenarios end-to-end
```

Replaced from 001: `planner/` is folded into `mind/`; `action.rs` →
`Command`/`Decision`/`Observation`; the old `Budget`/`TerminalReason` →
renewable `Budget` + `Termination`; `Agent::run` → `Brainstem::run`. Reused
verbatim: `tool/`, `provider/` (one additive change — `Api` carries `status`).

## Core contracts

```rust
// mind/mod.rs
#[async_trait] pub trait Mind: Send {
    async fn decide(&mut self, p: Perception) -> Decision;
    fn budget_summary(&self) -> BudgetSummary;          // read by the brainstem between decides (goal 12)
}
pub enum Perception { NewTask { goal: String }, Observation(Observation), Resume } // Clone; NewTask resets memory; Resume = continue after a throttle, no new stimulus (not folded)
pub enum Command  { CallTool { call_id: String, name: String, input: Value } }
pub enum Decision { Act(Command), Done(Outcome), Failed(Reason), Throttle(Instant) } // Instant = tokio::time::Instant
pub enum Reason   { Task(TaskFault), Service(AgentError) }                 // task-fatal vs service-fatal (goal 3)
pub enum TaskFault{ NoProgress, BudgetTooSmall, BadRequest(String), Malformed(String) }
pub struct BudgetSummary { pub tokens_remaining: u64, pub next_reset: Instant }

// observation.rs
pub enum Observation { ToolResult { call_id: String, output: Value }, Recoverable { call_id: Option<String>, error: RecoverableError } }
pub struct Outcome { pub message: String }
pub enum TaskOutcome { Completed(Outcome), Failed(TaskFault) }            // sent on Task.reply

// brainstem/mod.rs
pub struct Task { pub goal: String, pub reply: Option<oneshot::Sender<TaskOutcome>> }
pub enum Lifecycle { Idle, Working, Throttling, Cancelled, Fatal, Stopped }
pub struct Snapshot { pub lifecycle: Lifecycle, pub current_task: Option<String>,
                      pub tokens_remaining: u64, pub next_reset: Instant,
                      pub queue_depth: usize, pub steps_used: usize }
pub enum Termination { Cancelled, Fatal(AgentError), Stopped }           // run-level result of Brainstem::run

// budget.rs — pure fns of injected `now` (goal 15)
pub enum Period { Daily, Weekly, Every(Duration) }                       // Daily=24h, Weekly=7d
pub struct Budget { pub period: Period, pub max_tokens: u64 }
pub struct BudgetState { start: Instant, window: u64, used: u64 }
//  window(now)=floor((now-start)/period); refresh(now) rolls window & zeroes `used` on crossing (goal 15);
//  charge(now,t) = refresh then used = used.saturating_add(t)   // saturating — fixes the 001 overflow bug
//  remaining(now), exhausted(now), next_reset(now)=start+(window(now)+1)*period

// error.rs — classification drives goal 3
impl ProviderError { fn class(&self) -> ErrorClass } // Transient | ServiceFatal | TaskFatal
//  Transport|Decode|Api{429,5xx} -> Transient ; Api{401,403,404} -> ServiceFatal ; Api{400,422} -> TaskFatal
```

## ModelMind::decide (goals 2–5)

`ModelMind` carries a `resuming: bool` flag (cross-`decide` state) so a
throttle-resume can be distinguished from a fresh stimulus.

1. **Fold the perception once.** `NewTask` resets working memory and folds the
   goal; `Observation` appends a tool/error message; **`Resume` folds nothing**
   (the perception that preceded the throttle was already folded in the earlier
   `decide`). _(Fixes the duplicate-`Observation` bug: a throttle re-decide sends
   `Resume`, not the original perception, so working memory is never re-appended —
   for an at-start **or** a mid-decide throttle alike.)_
2. **Attempt loop** — each iteration may make one provider call:
   - `budget.refresh(now)`. If `budget.exhausted(now)` **before** a usable
     decision: if `self.resuming` (a *freshly reset* full window still cannot fund
     this decision) → `Failed(Reason::Task(BudgetTooSmall))` (goal 4); else set
     `self.resuming = true` and return `Throttle(budget.next_reset(now))`. _(The
     flag lives on the struct, so it survives the `Throttle` → sleep → re-`decide`
     boundary — fixing the unreachable-guard bug; SC 13 now fires when a fresh
     window re-exhausts.)_
   - Call the provider inside `tokio::time::timeout(per_call)`. Classify
     (`ProviderError::class`): Transient → `sleep(backoff)` (capped 60s, full
     jitter), emit `RetryScheduled`, retry **unbounded** (goal 3); ServiceFatal →
     `Failed(Reason::Service(_))`; TaskFatal → `Failed(Reason::Task(BadRequest))`.
   - On success: `budget.charge(now, usage)` (saturating). Map response:
     tool-calls → `Act(CallTool)`; final text → `Done`; **no usable command** →
     re-prompt, capped at 2 consecutive, then `Failed(Reason::Task(Malformed))`
     (goal 5); otherwise loop for the next call.
3. On returning any **terminal** decision (`Act`/`Done`/`Failed`) clear
   `self.resuming = false`.

## The drive loop (`Brainstem::run` → `Termination`)

Outer (idle) `select!` over `{ cancel.cancelled() → Cancelled, status_rx →
reply(snapshot), inbox.recv() → Some(task)=episode / None → Stopped }`.

`run_episode(task)`: `lifecycle=Working`, `steps=0`, `perception=NewTask`.
Per turn: **refresh the cached `Snapshot`** (from `BrainstemState` + `mind.budget_summary()`,
called while no decide borrow is held — goal 12); `tokio::pin!` the
`mind.decide(perception.clone())` future; loop a `biased` `select!` over
`{ cancel → return Cancelled, status_rx → reply(cached snapshot) and keep
looping (the pinned decide future is NOT dropped), &mut decide_fut → break with
the Decision }`. Destructure `self` into disjoint field borrows so the status
arm and the decide future borrow different fields. Then match the Decision:

- `Act(cmd)` → `steps+=1`; if `steps>max_steps` → emit+`TaskFailed(NoProgress)`,
  end episode (goal 8). Else actuate **off-loop** via
  `spawn_blocking(move || registry.execute(cmd))` (goal 7), wrapped in the same
  cancel/status `select!`; result → `Observation`; `perception=Observation(obs)`.
  Unknown tool → `Observation::Recoverable` (goal 13).
- `Done(o)` → emit `TaskCompleted`, `task.reply.send(Completed(o))`, end episode.
- `Failed(Task(f))` → emit `TaskFailed(f)`, reply `Failed(f)`, end episode —
  **service continues** (goal 10).
- `Failed(Service(e))` → return `Termination::Fatal(e)` (ends the run).
- `Throttle(t)` → `lifecycle=Throttling`, emit `ThrottleSleep{wake:t}`,
  `select!{ cancel → Cancelled, sleep_until(t) → () }`, then re-decide with
  `Perception::Resume` (goal 9) — **not** the original perception, so the mind
  does not re-fold it. No step consumed.

## RunEvent (goal 17)

`TaskReceived{goal}` · `Command{call_id,name}` · `CommandResult{call_id,ok}` ·
`Recovered{error}` · `RetryScheduled{attempt,delay,error}` · `TaskCompleted{outcome}` ·
`TaskFailed{reason}` · `WindowReset{window}` · `ThrottleSleep{wake}` ·
`Terminated{Termination}`.

## Test matrix (success criteria → test; `FakeMind`/`FakeProvider` + `start_paused`)

| SC | Test |
|----|------|
| 1 | FakeProvider scripts 503×2 then ok → assert `RetryScheduled`×2, episode proceeds, no Termination |
| 2 | FakeProvider 401 → `Termination::Fatal`, zero retries |
| 3 | 400 (and separately a step-liveness trip) → `TaskFailed`, next task still runs |
| 4 | malformed×2 recovered; ×3 → `TaskFailed(Malformed)`, service continues |
| 5 | tiny `max_tokens`; advance clock → `Throttle` then resume after reset; `used` zeroed |
| 6,12 | cancel mid-sleep (cancel before advancing past wake) and mid-decide (pending FakeMind) → `Cancelled` |
| 7 | two Tasks queued → two `TaskCompleted` events in order |
| 8 | drop all inbox senders → `Stopped` |
| 9 | Status query during Working → `Snapshot` with expected lifecycle/tokens/steps |
| 10 | command for an unregistered tool → `Observation::Recoverable`, episode continues |
| 11 | assert the event set is emitted across the above |
| 13 | `max_tokens` < one decide cost → `TaskFailed(BudgetTooSmall)`, service continues |

## Build order (commit per phase)

- **P1** `budget.rs` renewable window + unit tests (window roll, saturating charge, `next_reset`, exhaustion).
- **P2** `error.rs` classification + `ProviderError::Api{status}` + `provider/` update + tests.
- **P3** `mind/` types + `Mind` trait + `observation.rs` + `FakeMind` + tests.
- **P4** `mind/model.rs` `ModelMind` (decide, classify/retry/backoff, malformed cap, throttle, BudgetTooSmall) + unit tests (`FakeProvider`, `start_paused`).
- **P5** `brainstem/` `Brainstem::run` (idle/episode `select!`, pinned decide, `spawn_blocking` actuate, cancel, status, throttle) + `event.rs` + unit tests (`FakeMind`).
- **P6** `tests/actor_loop.rs` (SC 1–13) + `examples/service.rs`.
- **P7** `make check` green; update AGENTS.md module map + CLAUDE.md architecture.

## Decisions

- `Mind`/`Brainstem` are `#[async_trait]` trait objects (runtime dispatch, mirroring
  001's `Provider`/`Planner`). `Tool` stays sync; `ToolRegistry` becomes
  `Arc`-shared + `Send+Sync` for `spawn_blocking`.
- Time is `tokio::time::Instant`; `Budget` methods take `now` as a parameter
  (pure, goal 15) and the loop reads `Instant::now()` (pausable, goal 18) — no
  separate `Clock` trait (simplest design).
- Status is served from a **pre-built cached `Snapshot`** refreshed before each
  decide, so the status `select!` arm borrows a disjoint field and never drops
  the in-flight `decide` future (goal 12). The decide future is `pin!`ned across
  status replies.
- `ProviderError::Api` carries the HTTP status so the mind can classify
  service-fatal vs task-fatal vs transient (goal 3).
- Backoff + jitter are in-crate (seedable xorshift); tests fix the seed (or zero
  jitter) for determinism.
