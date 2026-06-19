# Spec 002 — actor agent: mind + spine (v0.2)

Status: **draft**. Supersedes the architecture and several normative rules of
[001](001-agent-core.md): the single `Agent::run` loop, the fatal-on-transport
rule, and the per-run hard-ceiling `Budget`. The 001 principles that survive:
errors are recoverable observations (now placed by layer), no command-specific
branching in the loop, typed boundaries, and the simplest design that satisfies
scope. Normative statements use RFC 2119 keywords; everything else is
_(informational)_.

## What it is _(informational)_

001 was a one-shot agent: a single goal, a single `Agent::run` loop owning
provider + planner + tools, terminating on the first transport error or any one
of three per-run ceilings. v0.2 reshapes it into a **perpetual actor service**
split into two components:

- **Mind** — cognition. Abstracts the LLM. Given a perception, decides the next
  command. Owns the provider, the planning translation, working memory, the
  token budget, and LLM-call resilience. Never touches the world.
- **Spine** — body + runtime. Owns the I/O boundary (a mailbox of incoming
  tasks), the peripherals (tool registry + execution), the drive loop,
  cancellation, step-liveness, and event emission. **The spine drives**: it
  senses a task, consults the mind for each decision, actuates commands on
  peripherals, feeds observations back, and repeats until the task ends — then
  pulls the next task. It runs forever until cancelled or a service-fatal error.

The agent reacts to external signals and repeats until cancelled or fatal
failure. The mind decides *what* to do; the spine decides *when to listen, act,
and halt*.

## Components & contract

### Mind (cognition) — the crate MUST…

1. **MUST** define a `Mind` trait:
   `async fn decide(&mut self, perception: Perception) -> Decision`, where
   `Decision` is one of `Act(Command)`, `Done(Outcome)`, `Failed(Reason)`.
2. A model-backed `Mind` **MUST** own the `Provider` (LLM) and map the model's
   native tool-calls into `Command::CallTool` and a final text answer into
   `Decision::Done`. _(This folds 001's `Planner` into the Mind.)_
3. The Mind **MUST** classify every provider error and own LLM-call resilience:
   - **Transient** (connection/network failure, per-call timeout, HTTP 429, HTTP
     5xx, body-`Decode`): retry with exponential backoff + jitter, internally,
     invisible to the spine. Retries are unbounded; only success or cancellation
     escapes.
   - **Service-fatal** (HTTP 401, 403, 404 — auth / endpoint-or-model config):
     return `Decision::Failed` flagged service-fatal; the spine **MUST** escalate
     it to run termination.
   - **Task-fatal** (HTTP 400, 422 — request shaped by this task's content, e.g.
     context too long): return `Decision::Failed` (task-scoped); the spine fails
     the task and keeps serving.
4. The Mind **MUST** own a **renewable token budget** (see Budget). When the
   current window's token quota is exhausted it **MUST** report a throttle to the
   spine carrying the next reset instant, rather than failing. _(The mind cannot
   think without tokens; it asks the spine to wait.)_
5. **Malformed model output** — a response yielding no valid command (neither
   text nor tool calls, or unparseable arguments) — **MUST** be treated as a
   recoverable cognitive condition: the Mind re-prompts the model with the error
   as context (001's "errors are observations," now internal to cognition),
   bounded so a model that keeps producing unusable output yields a **task-fatal**
   `Failed` rather than looping forever. It **MUST NOT** be treated as a transient
   transport retry and **MUST NOT** terminate the service.

### Spine (body + runtime) — the crate MUST…

6. **MUST** make the Spine the driver. It **MUST** own:
   - an **inbox** of incoming `Task`s, modeled as a `tokio::sync::mpsc`
     receiver (the standard tokio actor mailbox) — an in-memory channel for
     tests; real queues adapt by forwarding into the sender;
   - a **peripheral registry** (001's `ToolRegistry`) and command execution;
   - a **cancellation token** and the drive loop.
7. The drive loop **MUST**: pull a `Task`; run a **task episode** — repeatedly
   `mind.decide(perception)`, on `Act(cmd)` actuate via a peripheral and feed the
   resulting `Observation` back as the next perception, on `Done`/`Failed` end the
   episode — then emit the task result and pull the next task.
8. A task episode **MUST** be bounded by a per-task **step-liveness** budget
   (`max_steps`, fresh each task, never time-reset). Exceeding it ends the episode
   as a task-fatal `Failed(NoProgress)`, surfaced as an event. It **MUST NOT**
   terminate the service and **MUST NOT** sleep-and-resume. _(Its cause is
   non-convergence, not resource consumption — fundamentally unlike the token
   budget.)_
9. The Spine **MUST** honor the mind's token throttle (goal 4): suspend the loop
   and sleep until the reported reset instant, then resume the same episode. The
   sleep **MUST** be cancellable.
10. The Spine **MUST** run perpetually, terminating **only** on: cancellation
    (token) → `Cancelled`; a service-fatal mind error (goal 3) → `Fatal`; or the
    inbox closing / explicit shutdown → `Stopped`. Task completion or failure
    **MUST NOT** terminate the service.
11. **Cancellation MUST** be honored at any await — mid-decide, mid-actuate,
    mid-sleep — promptly aborting in-flight work; partial task memory is
    discarded.
12. The Spine **MUST** answer a **Status** query (a `oneshot` reply) with a
    snapshot: lifecycle state, in-flight task, tokens remaining + next reset,
    queue depth, steps used this task.
13. **No command-specific branching** in the drive loop: commands dispatch
    through the peripheral registry (001's rule, preserved). An unknown
    command/peripheral becomes a recoverable `Observation`, not a fatal error.

### Renewable budget — the crate MUST…

14. Model the token budget as a **renewable quota over a recurring window**:
    `period` ∈ `{ Daily, Weekly, Every(Duration) }`, with `max_tokens` per
    window. Windows are **fixed-from-start**: window *N* =
    `[start + N·period, start + (N+1)·period)`. Calendar/timezone alignment and
    token-bucket continuous refill are out of scope (see below).
15. The quota check **MUST** be a **pure function of injected time and
    consumption state** (preserving 001's testability); crossing a window
    boundary **MUST** reset that window's consumption to zero.
16. `max_steps` **MUST** be modeled separately from the token budget — it is the
    per-task liveness bound of goal 8, not a windowed resource quota.

### Signals — the crate MUST handle…

The agent reacts to these signals (handling defined above):

- **Task** (inbox) → run an episode.
- **Cancel** (token) → `Cancelled`, honored at any await.
- **inbox closed** (`recv()` returns `None`) → `Stopped`.
- **Status** (oneshot query) → reply with a snapshot.
- **token-exhausted** (internal, mind → spine) → throttle sleep until reset.
- **service-fatal error** (internal, from the mind) → `Fatal`.

### Observability — the crate MUST…

17. Emit `RunEvent`s covering: task received, decision/command, command result,
    recoverable observation, retry scheduled (mind), task completed, task failed,
    token-window reset, throttle-sleep (with wake time), and run termination
    (`Cancelled` / `Fatal` / `Stopped`).

## Out of scope for v0.2 — MUST NOT block it

- **Persistence** of budget-window state and agent memory across restarts →
  tracked in **issue #3**. v0.2 keeps both in memory: a restart refills the quota
  and loses the transcript.
- Calendar/timezone-aligned windows; token-bucket continuous refill.
- **Concurrent** task processing (v0.2 is sequential, one task at a time).
- Cross-task / long-term memory (per-task working memory only).
- Control signals beyond `Status`: `Shutdown{drain}`, `Pause`/`Resume`,
  `Reconfigure` — recognized as real-actor controls, deferred.
- Spine-side retry of flaky peripherals (tool failures stay recoverable
  observations the mind reasons about, per goal 13).
- Multi-window budgets; circuit-breakers; human-in-the-loop input beyond
  cancellation (still a future `Tool`, per 001).

## Success criteria

`make check` green, plus deterministic offline tests (a fake `Mind` and a fake
`Spine`/`Provider`, an injected clock, `tokio::test(start_paused)`) proving:

1. A transient LLM error (503 / dropped connection) → the mind retries with
   backoff → the episode proceeds; the service never terminates.
2. A service-fatal error (401) → the run terminates `Fatal`, with no retry.
3. A task-fatal error (400, or a step-liveness trip) → the task fails and the
   **service continues** to the next task.
4. Malformed model output → recovered within cognition, the episode continues;
   persistent unusable output → task-fatal, the service continues.
5. Token-window exhaustion → the spine throttles and resumes after the reset
   (driven by paused/advanced time), with consumption counters reset to zero.
6. **Cancellation** → `Cancelled`, including mid-sleep and mid-episode.
7. Two tasks are processed in sequence from the inbox; results are emitted as
   events.
8. inbox closed → `Stopped`.

## Stack _(informational)_

tokio actor pattern (`mpsc` inbox + `oneshot` replies +
`tokio_util::sync::CancellationToken`); an injectable clock so all
time-dependent logic stays unit-testable; in-crate exponential-backoff math (no
new dependency); **no date library** (fixed-from-start windows). `Mind` and
`Spine` are `#[async_trait]` trait objects for runtime dispatch; peripherals
(tools) stay sync. Fake `Mind` / fake `Spine` for tests, mirroring 001's
`FakeProvider`.

## Open questions _(informational)_

- Backoff defaults: base `1s`, ×2, cap `60s`, full jitter — confirm in the plan.
- The internal malformed-output retry cap (goal 5) — proposed `2`; finalize in
  the plan.
- Whether `Status` is required for the v0.2 MVP or also deferrable — included
  here for observability (a stated project value).
