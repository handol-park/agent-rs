# Spec 002 — actor agent: mind + brainstem (v0.2)

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
- **Brainstem** — body + runtime. Owns the I/O boundary (a mailbox of incoming
  tasks), the peripherals (tool registry + execution), the drive loop,
  cancellation, step-liveness, and event emission. **The brainstem drives**: it
  senses a task, consults the mind for each decision, actuates commands on
  peripherals, feeds observations back, and repeats until the task ends — then
  pulls the next task. It runs forever until cancelled or a service-fatal error.

The agent reacts to external signals and repeats until cancelled or fatal
failure. The mind decides *what* to do; the brainstem decides *when to listen, act,
and halt*.

## Types _(informational)_

A glossary so the contract below is unambiguous. Types are new to v0.2 unless
marked "from 001."

- **Task** — a unit of work pulled from the inbox: a goal plus optional metadata
  and an optional `oneshot` reply channel for its `TaskOutcome`.
- **Perception** — the single stimulus passed to `Mind::decide`:
  `Perception::Task(Task)` on the first decide of an episode (a new goal),
  `Perception::Observation(Observation)` on a subsequent decide, or
  `Perception::Resume` after a throttle (continue with the working memory
  unchanged — **no new stimulus**, so it is not folded; see goal 9). The brainstem
  passes only the latest stimulus, never the transcript; the mind accumulates
  perceptions into its own working memory. A `Perception::Task` marks a new
  episode and **resets** the mind's working memory.
- **Command** — an intention the mind emits for the brainstem to actuate, e.g.
  `Command::CallTool { name, input }`. Dispatched through the peripheral registry.
- **Observation** — the result of actuating a command: a tool result or a
  recoverable error (mapped from 001's `RecoverableError`), returned by the brainstem
  and fed back as the next Perception.
- **Decision** — the output of `Mind::decide`: one of `Act(Command)`,
  `Done(Outcome)`, `Failed(Reason)`, `Throttle(Instant)`.
- **Outcome** — a successful task result (the final answer text or data).
- **Reason** — why a task failed; carries whether it is **task-fatal** or
  **service-fatal**.
- **TaskOutcome** — `Completed(Outcome)` or `Failed(Reason)`; emitted per task,
  never run-terminal.
- **Lifecycle** — the brainstem's run state, reported in a `Snapshot`: `Idle`,
  `Working`, `Throttling`, and the terminal `Cancelled` / `Fatal` / `Stopped`.
- **Snapshot** — the `Status` reply: `Lifecycle` state, in-flight task, a budget
  summary (tokens remaining + next reset instant) **as of the last completed
  decision**, queue depth, and steps used this task.
- **From 001, surviving:** `Provider` (LLM transport), `Tool` / `ToolRegistry`
  (peripherals; sync), `RunEvent` (extended by goal 17), `RecoverableError`.
  001's `Planner` is absorbed into the `Mind`; 001's `Agent::run` and per-run
  `Budget` are replaced.

## Components & contract

### Mind (cognition) — the crate MUST…

1. **MUST** define a `Mind` trait:
   `async fn decide(&mut self, perception: Perception) -> Decision`, where
   `Decision` is one of `Act(Command)`, `Done(Outcome)`, `Failed(Reason)`,
   `Throttle(Instant)`. The mind **MUST** accumulate each `Perception` into its
   own working memory; the brainstem **MUST NOT** pass the transcript.
2. A model-backed `Mind` **MUST** own the `Provider` (LLM) and map the model's
   native tool-calls into `Command::CallTool` and a final text answer into
   `Decision::Done`. _(This folds 001's `Planner` into the Mind.)_
3. The Mind **MUST** classify every provider error and own LLM-call resilience:
   - **Transient** (connection/network failure, a per-call timeout, HTTP 429,
     HTTP 5xx, body-`Decode`): retry with exponential backoff + jitter. Defaults
     **MUST** be base `1s`, multiplier `2`, cap `60s`, full jitter. Each retry
     **MUST** emit a `RunEvent` (goal 17). Retries are intentionally
     **unbounded**: a run ends only on success, cancellation, or a service-fatal
     error — never on a transient one. The retry events and `Status` (goal 12)
     keep a persistently-failing provider observable. A single provider call
     **MUST** be bounded by a per-call timeout; a timed-out call is itself a
     transient error (and is retried). _(informational: a `Retry-After` header on
     429 / 503 is ignored in v0.2 — fixed exponential backoff is used regardless;
     honoring it is a future refinement, noted so it is not silently added later.)_
   - **Service-fatal** (HTTP 401, 403, 404 — auth / endpoint-or-model config):
     return `Failed` flagged service-fatal; the brainstem **MUST** escalate it to run
     termination.
   - **Task-fatal** (HTTP 400, 422 — request shaped by this task's content):
     return `Failed` (task-scoped); the brainstem fails the task and keeps serving.
   - **Unclassified status** — classification **MUST** be total (no provider
     response unhandled): any **5xx** not listed above **MUST** be transient
     (server-side, may self-heal); every **other** unlisted status (unlisted 4xx,
     and any 1xx/3xx surfaced as an error) **MUST** be task-fatal (the service does
     not blindly retry a request the server actively rejected).
4. The Mind **MUST** own a **renewable token budget** (see Budget). When the
   current window's token quota is exhausted, it **MUST** return
   `Decision::Throttle(reset_instant)` rather than `Failed`. The mind **MUST NOT**
   sleep — it reports the reset instant; the brainstem controls the wait (goal 9). If
   a single `decide` cannot complete within **one full window's** quota — it
   exhausts a freshly-reset window without producing `Act`, `Done`, or `Failed` —
   the mind **MUST** return a **task-fatal** `Failed` (the decision does not fit
   the budget) rather than throttling again. _(This bounds throttling and prevents
   an exhausted episode from sleeping forever.)_
5. **Malformed model output** — a response yielding no valid command (neither
   text nor tool calls, or unparseable arguments) — **MUST** be treated as a
   recoverable cognitive condition: the Mind re-prompts the model with the error
   as context (001's "errors are observations," internal to cognition). The Mind
   **MUST** cap this at **2 consecutive** malformed responses for one decision;
   on the third, it **MUST** return a task-fatal `Failed`. _(The bound is
   normative so a model producing endless garbage cannot spin invisibly; each
   re-prompt also draws on the token budget.)_ It **MUST NOT** be treated as a
   transient transport retry and **MUST NOT** terminate the service.
   _(informational: malformed output is distinct from a body-`Decode` failure
   (goal 3, transient). Malformed means the body **decoded** but its content is
   unusable — so the Mind has something to feed back into an **informed re-prompt**,
   bounded at 2. `Decode` means the body never parsed — there is nothing to show the
   model, so the only retry is a **blind re-issue** of the identical request, which
   is exactly transient backoff. Different mechanism, hence different layer.)_

### Brainstem (body + runtime) — the crate MUST…

6. **MUST** make the Brainstem the driver. It **MUST** own:
   - an **inbox** of incoming `Task`s, modeled as a `tokio::sync::mpsc`
     receiver (the standard tokio actor mailbox) — an in-memory channel for
     tests; real queues adapt by forwarding into the sender;
   - a **peripheral registry** (001's `ToolRegistry`) and command execution;
   - a **cancellation token** and the drive loop.
7. The drive loop **MUST**: pull a `Task`; run a **task episode** — pass
   `Perception::Task` to `mind.decide`, then on `Act(cmd)` actuate via a
   peripheral and pass the resulting `Observation` back as
   `Perception::Observation`; on `Throttle(t)` wait until `t` (goal 9) and
   continue the same episode; on `Done`/`Failed` end the episode — then emit the
   `TaskOutcome` and pull the next task. Because 001's `Tool` is synchronous, tool
   actuation **MUST** run off the drive loop's task (on a blocking thread) so a
   long tool call cannot block cancellation or Status handling (goal 11).
8. A task episode **MUST** be bounded by a per-task **step-liveness** budget
   (`max_steps`, fresh each task, never time-reset), counted per `mind.decide`
   that returns `Act`. Exceeding it ends the episode as a task-fatal
   `Failed(NoProgress)`, surfaced as an event. It **MUST NOT** terminate the
   service and **MUST NOT** sleep-and-resume. _(Its cause is non-convergence, not
   resource consumption — fundamentally unlike the token budget. The mind's
   internal malformed re-prompts (goal 5) are bounded separately and do not
   consume steps; throttling cannot loop forever because a window too small for
   one decision is task-fatal, goal 4.)_
9. The Brainstem **MUST** honor `Decision::Throttle(t)`: suspend the loop and sleep
   until `t`, then resume the episode with `Perception::Resume` — the working
   memory is **not** re-folded, and the mind **MUST** treat a second exhaustion of
   a freshly-reset window as task-fatal (goal 4), so throttling cannot loop
   forever. The sleep **MUST** be cancellable.
10. The Brainstem **MUST** run perpetually, terminating **only** on: cancellation
    (token) → `Cancelled`; a service-fatal mind error (goal 3) → `Fatal`; or the
    inbox closing (`recv()` returns `None`) → `Stopped`. Task completion or
    failure **MUST NOT** terminate the service. _(Graceful `Shutdown`/drain is out
    of scope; inbox-close is the only `Stopped` path in v0.2.)_
11. **Cancellation MUST** be honored while a decision or actuation is in flight,
    not only between them: the brainstem **MUST** structure its drive loop so the
    cancellation token and the Status query are serviced **concurrently** with the
    in-flight `mind.decide`, actuation, or throttle sleep. Combined with goal 7
    (actuation off the loop), this makes every long operation interruptible.
    Cancellation discards in-flight work and partial task memory. _(informational:
    e.g. a `select!` over the in-flight future, the cancel token, and the Status
    channel; the exact structure is a plan concern.)_
12. The Brainstem **MUST** answer a **Status** query (a `oneshot` reply) with a
    `Snapshot`. The Snapshot **MUST** be built from **brainstem-owned cached state** —
    including a budget summary the mind reports after each decision — and **MUST
    NOT** require borrowing the mind while a `decide` is in flight; its budget
    fields therefore reflect state as of the last completed decision. _(Status is
    in the v0.2 MVP: a perpetual service must be observable — a stated project
    value.)_
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
    boundary **MUST** reset that window's consumption to zero. Consumption **MUST**
    be sampled **per provider call** and charged against the window current when
    that call completes; if a single decision's calls straddle a reset, the later
    calls count against the new window.
16. `max_steps` **MUST** be modeled separately from the token budget — it is the
    per-task liveness bound of goal 8, not a windowed resource quota.

### Signals — the crate MUST handle…

The agent reacts to these signals (handling defined above):

- **Task** (inbox) → run an episode.
- **Cancel** (token) → `Cancelled`, honored at any await.
- **inbox closed** (`recv()` returns `None`) → `Stopped`.
- **Status** (oneshot query) → reply with a `Snapshot`.
- **token-exhausted** (internal) → the mind returns `Decision::Throttle`; the
  brainstem sleeps until the reset instant.
- **service-fatal error** (internal, from the mind) → `Fatal`.

### Observability — the crate MUST…

17. Emit `RunEvent`s covering: task received, decision/command, command result,
    recoverable observation, retry scheduled (mind), task completed, task failed,
    token-window reset, throttle-sleep (with wake time), and run termination
    (`Cancelled` / `Fatal` / `Stopped`).

### Determinism — the crate MUST…

18. All time-dependent waits — backoff, the per-call provider timeout, and the
    throttle sleep — and the budget-window clock **MUST** use `tokio::time`, so
    every time-dependent MUST is deterministic under `tokio::test(start_paused)`.
    The injectable clock **MUST** be backed by `tokio::time::Instant`; no MUST may
    depend on real wall-clock.

## Out of scope for v0.2 — MUST NOT block it

- **Persistence** of budget-window state and agent memory across restarts →
  tracked in issue #3 (`github.com/handol-park/agent-rs/issues/3`). v0.2 keeps
  both in memory: a restart refills the quota and loses the transcript.
- Calendar/timezone-aligned windows; token-bucket continuous refill.
- **Concurrent** task processing (v0.2 is sequential, one task at a time).
- Cross-task / long-term memory (per-task working memory only).
- Control signals beyond `Status`: `Shutdown{drain}`, `Pause`/`Resume`,
  `Reconfigure` — recognized as real-actor controls, deferred.
- Per-tool actuation timeout (sync tools cannot be preempted; cancellation is the
  only interrupt — goals 7, 11).
- Brainstem-side retry of flaky peripherals (tool failures stay recoverable
  observations the mind reasons about, per goal 13).
- Multi-window budgets; circuit-breakers; human-in-the-loop input beyond
  cancellation (still a future `Tool`, per 001).

## Success criteria

`make check` green, plus deterministic offline tests (a fake `Mind` and a fake
`Brainstem`/`Provider`, an injected clock, `tokio::test(start_paused)`) proving:

1. A transient LLM error (503 / dropped connection / a body-`Decode` failure) →
   the mind retries with backoff, **emitting a retry event per attempt** → the
   episode proceeds; the service never terminates. _(The `Decode` case verifies it
   is classified transient — a blind re-issue — not task-fatal.)_ _Determinism:_ because the clock is paused, the test
   **MUST** also assert the backoff is **exponential** — after retry attempt *N*,
   advancing the clock by **less than** the expected delay yields no further
   attempt, and advancing past it yields the next — so the timing math is covered,
   not just the event count.
2. A service-fatal error (401) → the run terminates `Fatal`, with no retry.
3. A task-fatal error (400, or a step-liveness trip) → the task fails and the
   **service continues** to the next task.
4. Two consecutive malformed responses are recovered within cognition and the
   episode continues; a third consecutive malformed response → task-fatal, and
   the service continues (verifies the goal-5 normative cap).
5. Token-window exhaustion → the mind returns `Throttle` and the brainstem sleeps
   (driven by paused/advanced time) and resumes after the reset, with consumption
   counters reset to zero.
6. **Cancellation** → `Cancelled`, including mid-episode and mid-sleep.
   _Determinism:_ the cancel is signalled **before** the paused clock is advanced
   past the wake instant, so the cancel branch wins deterministically over the
   timer.
7. Two tasks are processed in sequence from the inbox; each `TaskOutcome` is
   emitted as an event.
8. inbox closed → `Stopped`.
9. A **Status** query returns a `Snapshot` with the correct lifecycle state,
   tokens remaining, and steps used (verifies goal 12) — including a query
   **during a throttle sleep**, which **MUST** be answered (not blocked) and
   **MUST** report `Throttling` (verifies goal 11's "concurrently with … the
   throttle sleep").
10. An **unknown command** yields a recoverable `Observation` and the episode
    continues (verifies goal 13's registry dispatch with no command-branching).
11. The documented `RunEvent`s (goal 17) are emitted on their paths — at minimum
    task-received, command, command-result, **recoverable-observation**,
    retry-scheduled, **window-reset**, throttle-sleep, task-completed/failed, and
    the terminal reason — asserted within the tests above. _(The window-reset path
    is exercised by SC 5 and the recoverable-observation path by SC 10.)_
12. Cancellation honored **mid-decide**: cancelling while a `decide` future is in
    flight terminates `Cancelled`; because actuation runs off the loop (goal 7), a
    cancel during a tool call is likewise observed.
13. A window that cannot fund a single provider call (`max_tokens == 0`) → the
    decision returns task-fatal `Failed` and the service continues (verifies
    goal 4's no-deadlock rule). _(informational: the exhaustion check is **before**
    each call and a call's cost is unknown until it returns, so a window with
    `max_tokens > 0` is never exhausted at the top of a fresh `decide` — its first
    call always proceeds and may overspend once; thus the smallest deterministic
    trigger for this criterion is a zero-quota window.)_ The test **MUST** assert
    the **intermediate `Throttle`** first — the initial `decide` returns `Throttle`
    with **no provider call made**, a `ThrottleSleep` event is emitted, the clock
    is advanced past the reset, and only the `decide(Resume)` against the
    freshly-reset-but-still-zero window returns task-fatal — so an implementation
    that skips the throttle and fails on first exhaustion (violating goal 4's
    wait-then-decide order) is rejected.

## Stack _(informational)_

tokio actor pattern (`mpsc` inbox + `oneshot` replies +
`tokio_util::sync::CancellationToken`); an injectable clock backed by
`tokio::time` so all time-dependent logic stays unit-testable under
`start_paused`; in-crate exponential-backoff math (no new dependency); **no date
library** (fixed-from-start windows). `Mind` and `Brainstem` are `#[async_trait]`
trait objects for runtime dispatch; peripherals (tools) stay sync and actuate on
a blocking thread (goal 7). Fake `Mind` / fake `Brainstem` for tests, mirroring 001's
`FakeProvider`.

## Open questions _(informational)_

None outstanding. The three prior items are resolved normatively above: `Status`
is in the MVP (goal 12); backoff defaults are fixed (goal 3); the malformed-output
retry cap is fixed at 2 (goal 5).
