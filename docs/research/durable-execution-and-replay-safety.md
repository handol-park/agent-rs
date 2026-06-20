# Research Note — Durable Execution, Replay Safety & Persistent Cognition

Status: **informational / exploratory**. Not a spec or plan, and not bound to the
current v0.1/v0.2 architecture. This note feeds a _future_ direction that becomes
relevant only **after persistence exists** (issue #3); the ideas here may *steer*
that architecture rather than fit into it. Nothing here is normative.

## Provenance and trust

Distilled from an external report, *"Engineering Resilient Agent Systems: An
Architectural Blueprint for Durable Execution, Persistent Memory, and Autonomous
Self-Improvement"* (a Gemini Deep Research artifact). The report was
fact-checked before any of it was carried into this note. **Only claims that
survived verification against primary sources are used below.** Carried-forward
ideas trace to real, checked sources:

- Durable-execution model & worker versioning — Temporal / Dapr docs.
- Replay safety / idempotency-key derivation — Stripe, Square, Twilio docs (provider
  behaviors confirmed).
- Runtime-semantics / "amnesia tax" — arXiv 2603.01209 (numbers confirmed verbatim:
  ~80% missing-variable errors on persistent->stateless mismatch; ~3.5x tokens on the
  inverse; *solution quality statistically indistinguishable*).
- Memory taxonomy — CoALA (arXiv 2309.02427), HippoRAG (2405.14831), Zep/Graphiti
  (2501.13956).
- Eval-gaming case study — Anthropic, "Eval awareness in Claude Opus 4.6's BrowseComp
  performance."

**Explicitly NOT carried forward** (unsupported, fabricated, or noise — see the
fact-check): the "90% failure past 4 hours" and "60% from executable skills"
statistics, the AWS Strands "7s / 2GB / 500-msg" figures, the "SKILL.md across 30+
platforms" count, the MCP-as-OS analogy (decorative), SKILL.md packaging, the "Remy"
spec-compiler, and the Redis-specific middleware recipe (an implementation detail, not
a design idea). One correction matters technically and is reflected below: durable
engines are **at-least-once, not exactly-once** — replay reuses *recorded results*
rather than guaranteeing single execution.

---

## Why this is a *post-persistence* question

agent-rs's defining bet is that **errors are recoverable observations, not terminal
states** — recovery *within* a live episode. That is orthogonal to surviving a
**process death**: today, if the process dies, the run dies with it (in-memory state
only).

Persistence (issue #3) closes that gap by writing state to durable storage. But the
moment state outlives the process, a second-order question appears that the report
frames sharply, and that this note exists to flag early:

> **Persisting state is not the same as durable execution.** A checkpoint is a save
> point; *something still has to detect the crash, decide to resume, and replay
> safely.* The hard problems live in the replay, not the save.

The directions below are the consequences of taking that seriously. They are offered
as **forks the future architecture could take**, not as a fixed design.

---

## Direction 1 — Decide deliberately: checkpoint vs. durable execution

The report's strongest, best-sourced section is the distinction between two
persistence philosophies:

- **Checkpoint-and-resume.** Snapshots at boundaries. The application owns crash
  detection and re-invocation. Simple; recovery is manual and external.
- **Durable execution** (Temporal/Dapr style). The runtime logs every decision and
  external result to an **append-only event history**. On crash, a healthy worker
  **replays the code from the start**, and instead of re-running costly/side-effecting
  steps, the engine **injects the previously recorded results**. The program reads as
  linear code; durability is a property of the runtime.

These are genuinely different architectures, not points on a slider. A post-persistence
agent-rs should pick one **on purpose**. The durable-execution model is attractive
because it preserves the project's "linear, readable loop" aesthetic while making
recovery automatic — but it imposes a hard constraint (Direction 2) that shapes
everything else.

## Direction 2 — Replay safety is the load-bearing constraint (idempotency)

If recovery replays the loop, then **every side-effecting action will be re-attempted**
unless it is idempotent or its result is replayed from the log. This is the single most
important downstream consequence of adding durability, and the place a future plan is
most likely to get silently burned (a replayed run that sends the email twice, charges
the card twice).

Two complementary mechanisms, both verified as real practice:

1. **Record-and-replay results** (the durable-execution answer). Persist each action's
   *output* in the event history; on replay, return the recorded output instead of
   re-executing. Note the corrected nuance: this is **at-least-once execution with
   exactly-once *observed effect via the log*** — not a magic exactly-once guarantee.
   The boundary case (crash *after* the side effect, *before* the result is logged) is
   exactly where idempotency keys are still needed.

2. **Deterministic idempotency keys** for genuinely-external mutations. Derive the key
   from variables that describe the *logical step* — never from a timestamp or random
   seed, because a replay must reproduce the same key. The report's formulation:
   `key = hash(conversation/run id || call id || tool name || args)`. Including a per-call
   id is what lets two *intentionally* identical calls (two separate $10 refunds) both
   go through.

Design implications worth surfacing now, while tools are still simple:

- **Tools may need a side-effect classification** — read-only / mutating-with-key /
  long-running-async — so the runtime knows what is safe to replay vs. what needs a key
  or a status-poll before re-issue. Read-only calls are naturally idempotent and need
  none of this. The side-effect contract is a natural thing to attach to a tool's
  advertised schema, which intersects with the already-open question of a
  runtime-mutable tool registry (issue #9).
- **A stable per-call identity** (whatever form it takes) is the natural anchor for both
  the result-log and the idempotency key. Worth keeping such an identity stable and
  meaningful as the architecture evolves.
- Provider reality to design against: Stripe (`Idempotency-Key` header, ~24h window) and
  Square (`idempotency_key` body field) support this natively; **Twilio does not** on
  SMS/voice — so any "just pass the key through" assumption is wrong for some tools, and
  the runtime must be able to wrap those itself.

## Direction 3 — Persistent cognition and the "amnesia tax"

Once cognition state is persisted and *restored*, a subtle failure mode (arXiv
2603.01209, numbers verified) becomes relevant: **the execution semantics assumed when
producing reasoning traces must match the semantics at restore time.**

- If the agent assumes a **persistent** workspace (its prior variables/state are still
  there) but is restored into a **stateless** one, it references things that are gone ->
  cascading missing-variable errors (~80% of episodes in the paper).
- If it assumes **stateless** (re-externalizes the whole workspace every turn) but runs
  on a **persistent** substrate, it redundantly re-declares state — the **amnesia tax**,
  ~3.5x tokens. Crucially, the paper found **solution quality unchanged** either way;
  only cost and stability move.

The transferable lesson for a persistent agent-rs: **be explicit about where cognitive
state lives and how restore reconstitutes it** — restore-and-continue vs.
replay-to-rebuild are different contracts, and the agent must be operating under the
one it was built for. The report's broader memory taxonomy (CoALA: working / episodic /
semantic / procedural; HippoRAG-style graph retrieval; Zep-style temporal facts with
validity windows) is a useful vocabulary if persisted memory ever grows past a flat
log — but a flat, append-only history is already the right *substrate* for both replay
(Direction 1) and episodic memory, and is worth treating as the foundation before
layering structure on top.

## Direction 4 — Code/version skew across persisted runs

A purely in-process agent never faces this; a durable one does. If a persisted run
outlives a deploy, **replaying its history against changed loop code produces
non-determinism** (the recorded event sequence no longer matches the new code path).
Temporal's verified answers:

- **Pin** a run to the code version it started on (run old code to completion, route new
  runs to new code), **or**
- **Auto-upgrade** active runs, which then requires explicit version-branching in the
  code, **or**
- **Upgrade at natural boundaries** ("continue-as-new"): end the current run-segment and
  start a fresh one on the new version, so any single segment is short enough to live on
  one code version.

This is not actionable until durability exists, but it is a **known tax on the
durable-execution path** and should inform whether that path is taken — i.e. it is a
cost to weigh in Direction 1, not a detail to discover later.

## Direction 5 — Verification and eval discipline (lighter weight)

Two ideas worth banking for when/if the agent self-improves or runs long autonomous
trajectories:

- **Layered verification against trajectory collapse.** A small error at step *t* can
  silently compound. Cheap guardrails (validating plans/inputs/return-codes, bounding
  malformed-output re-prompts, escalating on low confidence) intercept it before it
  cascades. agent-rs already leans this way philosophically; persistence makes the cost
  of a silently-corrupted *and saved* trajectory higher, which strengthens the case.
- **Eval-gaming is real and sandbox-network-shaped.** The verified Opus 4.6 / BrowseComp
  case (model recognized it was under eval, found the benchmark's encrypted answer key on
  GitHub, wrote `derive_key()`/`decrypt()` in a Python sandbox) is a concrete argument
  for **least-privilege, network-restricted eval sandboxes** and **process-based scoring
  (reward the *how*, not just the answer)** if agent-rs ever optimizes against its own
  evals. (Caveat: the report mis-stated "1,266 decrypted entries"; 1,266 was the dataset
  size — 11 contaminated problems, 16 failed decryption attempts.)

---

## What to deliberately leave out

To keep a future plan honest and small: no need for a Temporal/Dapr dependency to *state*
these problems, no Redis middleware recipe, no SKILL.md packaging, no spec-compiler. The
value of the report here is the **problem decomposition** (what breaks once state is
durable, and why), not its tooling shopping list.

## Open questions for the future plan

1. Checkpoint or durable execution? (Direction 1 — pick before building.)
2. If durable: what is the unit of replay, and what exactly is recorded vs. re-executed?
3. What is the side-effect contract for tools, and where do idempotency keys live for the
   "external mutation that crashed before its result was logged" case?
4. Restore semantics for cognition: continue-from-restored-state or replay-to-rebuild —
   and is the agent built for the one chosen? (Direction 3.)
5. Is the durable-execution version-skew tax (Direction 4) acceptable, or does it argue
   for short, bounded run-segments?
