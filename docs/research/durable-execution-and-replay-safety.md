# Research Note — Durable Execution, Replay Safety & Persistent Cognition

Status: **informational, exploratory, and non-normative**. This is not a spec or
plan. It identifies design questions that become relevant if agent-rs adds
cross-process persistence.

## Provenance and trust

Primary-source statements below are linked to vendor documentation or papers.
Statements about how agent-rs could apply those facts are explicitly presented as
**author synthesis** or a **design option**. The sources establish how the cited
systems behave; they do not prescribe an agent-rs architecture.

## Current agent-rs context

The current v0.2 service separates cognition (`Mind`) from runtime orchestration
(`Brainstem`). `Brainstem` accepts tasks and drives each task's episode, dispatching
tool work identified by `Command::CallTool { call_id, ... }`; `ModelMind` owns working
memory and a `RenewableBudget`.

Recovery inside a live episode is not recovery from process death. Today these
components and their task state are in memory. Durable recovery would require a
persisted representation plus an owner that detects interruption and resumes work.

## 1. Checkpointing and replay can be combined

Checkpointing and replay-based durable execution are strategies that can be used
separately or together:

- A checkpoint stores a state snapshot from which application or platform code can
  resume.
- Event-sourced durable runtimes store history and reconstruct orchestration state by
  deterministic replay. Temporal records workflow history and checks replayed commands
  against it ([workflow execution](https://docs.temporal.io/workflow-execution),
  [event history](https://docs.temporal.io/encyclopedia/event-history)); Dapr and
  Durable Task describe equivalent replay from append-only history
  ([Dapr](https://docs.dapr.io/developing-applications/building-blocks/workflow/workflow-features-concepts/),
  [Durable Task](https://learn.microsoft.com/en-us/azure/durable-task/common/durable-task-orchestrations)).
- Snapshots can shorten recovery while events preserve auditability or permit
  reconstruction. Activity-level checkpoints can also coexist with orchestration
  history; Temporal, for example, documents heartbeat details as a way to resume a
  later activity attempt
  ([activities](https://docs.temporal.io/activities)).

**Author synthesis:** agent-rs should choose these dimensions explicitly rather than
make a binary “checkpoint or replay” choice:

1. **Recovery ownership:** caller, agent-rs supervisor, or external durable runtime.
2. **Durable representation:** snapshots, events, or both.
3. **Replay unit:** command, task episode, bounded segment, or whole execution.
4. **Side-effect protocol:** recorded result, idempotency key, reconciliation,
   compensation, or manual intervention.
5. **Version policy:** compatible replay, version-routed workers, migration, or
   termination/restart.

## 2. Replay safety and activity semantics

During orchestration replay, a completed activity's recorded result is reused rather
than the completed activity being scheduled again
([Dapr](https://docs.dapr.io/developing-applications/building-blocks/workflow/workflow-features-concepts/),
[Durable Task](https://learn.microsoft.com/en-us/azure/durable-task/common/durable-task-orchestrations)).
That does not make external effects exactly once. If a worker performs an effect and
crashes before completion is durably recorded, the runtime may attempt the activity
again. Dapr therefore guarantees activity execution **at least once**, and Temporal
recommends idempotent writes
([activity definition](https://docs.temporal.io/activity-definition)).

Read-only operations are not automatically replay-safe. A read can be
non-deterministic, consume a metered or rate-limited service, expose changing data, or
trigger provider-side effects. Temporal treats API calls, database queries, and LLM
calls as activity work outside deterministic workflow code
([workflow definition](https://docs.temporal.io/workflow-definition)).

For agent-rs, `Command::CallTool::call_id` is a plausible logical-operation identity,
but any durable meaning must be specified. A future tool contract may need to describe
retry behavior, idempotency support, reconciliation, and whether results are safe and
appropriate to retain. Issue #9 is related only as a tool-schema refresh constraint;
it does not currently track side-effect metadata.

## 3. Idempotency keys are a design option, not a derived fact

**Agent-rs design option:** derive or otherwise persist one key per logical tool
operation, anchored by stable execution identity and `call_id`. The important provider
contract is not deterministic derivation itself: generate a sufficiently unique key
once for the logical operation, then reuse that same key and request parameters for
retries.

Provider behavior is endpoint-specific:

- Stripe accepts idempotency keys for `POST` requests, reuses the first saved result
  for the same key, and says keys may be removed after they are at least 24 hours old
  ([idempotent requests](https://docs.stripe.com/api/idempotent_requests),
  [retry guidance](https://docs.stripe.com/error-low-level#idempotency)).
- Square allows supporting API operations to accept a unique idempotency key and
  return the prior result for a duplicate request. Its general guidance does not
  document a universal retention duration
  ([Square idempotency](https://developer.squareup.com/docs/build-basics/common-api-patterns/idempotency)).
- Twilio's current Message and Call creation references expose no general
  client-supplied idempotency field
  ([Message](https://www.twilio.com/docs/messaging/api/message-resource),
  [Call](https://www.twilio.com/docs/voice/api/call-resource)). This is a statement
  about those documented creation APIs, not a claim that every Twilio product lacks
  idempotency support.

Provider retention limits mean an old replay may require lookup, reconciliation,
compensation, or operator review rather than blind resubmission.

## 4. Runtime semantics and persistent cognition

[Agents Learn Their Runtime](https://arxiv.org/abs/2603.01209) studies Qwen3-8B models
fine-tuned and evaluated on the paper's Opaque Knapsack benchmark under persistent and
stateless Python-interpreter conditions. In that experiment, train/runtime mismatch
changed token cost and failure behavior. Reported solution-quality differences were
not statistically significant; that does not establish equality or generalize to
other models, benchmarks, or agent architectures. The paper studies interpreter
persistence across tool steps, not restoration after a process crash.

[CoALA](https://arxiv.org/abs/2309.02427) remains useful vocabulary for working,
episodic, semantic, and procedural memory. HippoRAG and Zep illustrate graph-oriented
and temporal retrieval designs
([HippoRAG](https://arxiv.org/abs/2405.14831),
[Zep](https://arxiv.org/abs/2501.13956)).

**Architectural hypothesis:** a durable execution history could also contribute to
episodic memory. It should not be assumed that an execution log is already a useful
memory system. Current `RunEvent` values are an in-process observability stream, not a
serializable durable-history schema. A future durable model would need explicit
decisions about event content, schema versioning, retention, redaction, indexing, and
retrieval.

## 5. Version skew and bounded histories

Replay requires workflow code to remain deterministic relative to recorded history.
Temporal documents two versioning strategies—Worker Versioning and patching—which may
be combined
([workflow definition](https://docs.temporal.io/workflow-definition)). Dapr likewise
warns that updates must preserve determinism for incomplete workflows
([features and concepts](https://docs.dapr.io/developing-applications/building-blocks/workflow/workflow-features-concepts/)).

Continue-As-New starts a new run with fresh history. It can bound history size and,
as an agent-rs design choice, could bound migration scope. It is not an equivalent
third mechanism for making incompatible code replay old history safely.

## 6. Verification and evaluation discipline

**Author synthesis:** persistent systems raise the cost of accepting a corrupted
trajectory because bad state can survive the process that produced it. Validate
inputs, command results, and durable writes; preserve evidence needed for audit or
reconciliation; and evaluate agents in environments whose tools and privileges match
the intended deployment. This general lesson does not depend on a specific vendor or
benchmark case study.

## Open questions

1. Who owns recovery, and what state defines a resumable task episode?
2. Are snapshots, events, or both durable, and what is the replay unit?
3. Which tool effects can be retried, reconciled, compensated, or only reviewed?
4. How are logical operation identities and provider idempotency windows handled?
5. What history and code-version boundaries keep replay and migration tractable?
6. Is durable history solely operational evidence, or also an episodic-memory input?

## References

- Temporal: [Workflow execution](https://docs.temporal.io/workflow-execution);
  [Event history](https://docs.temporal.io/encyclopedia/event-history);
  [Workflow definition and versioning](https://docs.temporal.io/workflow-definition);
  [Activity definition](https://docs.temporal.io/activity-definition);
  [Activities](https://docs.temporal.io/activities)
- Dapr: [Workflow architecture](https://docs.dapr.io/developing-applications/building-blocks/workflow/workflow-architecture/);
  [Workflow features and concepts](https://docs.dapr.io/developing-applications/building-blocks/workflow/workflow-features-concepts/)
- Microsoft: [Durable Task orchestrations](https://learn.microsoft.com/en-us/azure/durable-task/common/durable-task-orchestrations)
- Stripe: [Idempotent requests](https://docs.stripe.com/api/idempotent_requests);
  [Idempotency and retries](https://docs.stripe.com/error-low-level#idempotency)
- Square: [Idempotency](https://developer.squareup.com/docs/build-basics/common-api-patterns/idempotency)
- Twilio: [Message resource](https://www.twilio.com/docs/messaging/api/message-resource);
  [Call resource](https://www.twilio.com/docs/voice/api/call-resource)
- Research: [Agents Learn Their Runtime](https://arxiv.org/abs/2603.01209);
  [CoALA](https://arxiv.org/abs/2309.02427);
  [HippoRAG](https://arxiv.org/abs/2405.14831);
  [Zep](https://arxiv.org/abs/2501.13956)
