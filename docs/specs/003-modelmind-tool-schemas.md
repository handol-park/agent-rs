# Spec: Wire tool schemas into ModelMind

Status: draft
Branch: `feat/003-modelmind-tool-schemas`
Issue: #8

## Goal

Make `ModelMind` advertise the registry's tool schemas to the provider so a real
LLM can emit native tool calls — unblocking the spec-002 actor flow
**Act → actuate → Observation → decide** against a live provider, not just
`FakeMind`.

## Why

`ModelMind::call_with_retry` builds every `ModelRequest` with
`tools: Vec::new()` (`src/mind/model.rs:102`, `// TODO: pass tool schemas`). A
real LLM is therefore never told which tools exist, so it can never emit a
native tool call. Against a live provider only `Decision::Done` / malformed /
failed are reachable — the whole point of the actor agent (decide → call a tool
→ observe the result → decide again) is dead code on the real path.

The response-mapping side (model tool-call → `Command::CallTool`) already exists
and is covered by `FakeMind` tests. The `ToolSchema` type
(`src/tool/mod.rs:30`) and `ToolRegistry::schemas()` (`src/tool/mod.rs:70`)
already exist, and `ModelRequest.tools: Vec<ToolSchema>`
(`src/provider/mod.rs:25`) is already the right shape. The only missing wire is
**getting the schemas from the registry (owned by the brainstem) into the
ModelMind so it can populate the request.**

_(informational)_ Surfaced by Claude's review of PR #7. Spec-002's success
criteria are all `FakeProvider`/`FakeMind`-driven, so the empty-schema path was
invisible to the suite.

## Success Criteria

Numbered list of falsifiable outcomes. Each item should be testable.

1. The `Mind` trait MUST expose a `set_tools(&mut self, tools: Vec<ToolSchema>)`
   hook with a default no-op body, mirroring `set_event_sink`. Minds that need
   no schemas (e.g. `FakeMind`) MUST compile unchanged.
2. `ModelMind` MUST retain the injected schemas and include them in **every**
   `ModelRequest` it sends (initial call and every retry), replacing the
   hard-coded `tools: Vec::new()`. A `ModelMind` that was never given schemas
   MUST still send an empty `tools` vec (no panic, no `unwrap`).
3. The `Brainstem` MUST inject `self.registry.schemas()` into the mind at
   `run()` start, alongside the existing `set_event_sink` call, so the mind's
   advertised tools match the registry the brainstem dispatches against.
4. An integration test MUST drive a real `ModelMind` + `Brainstem` against a
   scripted provider that returns a native tool call, and assert the full
   loop reaches the tool's effect: the scripted tool call is actuated through
   the `ToolRegistry`, its result is folded back as an `Observation`, and a
   subsequent provider turn returns a final answer (`Decision::Done`). The same
   test (or a focused unit test) MUST assert the provider received the tool
   schemas in its request (e.g. via `FakeProvider::requests_handle()`), proving
   the schemas actually reached the wire.
5. `make check` (fmt + clippy `-D warnings` + test) MUST pass.

## Out of Scope

Explicitly list what this spec does NOT cover to prevent scope creep.

- Parallel / multiple tool calls per turn (already deferred in spec-002;
  `ModelMind` continues to act on the first tool call only).
- Changing the `ToolSchema` shape, the `ToolRegistry`, or how providers
  serialize tools onto the wire (the OpenAI adapter already serializes
  `ModelRequest.tools`).
- The hard-coded system prompt (`"You are a helpful assistant."`) — improving
  the prompt is a separate concern.
- Per-task or dynamic tool subsets — the mind advertises the full registry for
  the whole run.

## Open Questions

Questions that must be resolved before or during implementation.

1. Injection mechanism: `set_tools` hook (mirrors `set_event_sink`, no
   constructor churn, keeps `FakeMind` untouched) vs. a `ModelMind::new`
   parameter. **Resolved (informational): the `set_tools` hook**, per the
   issue's preferred direction and symmetry with the existing event-sink
   injection — the brainstem already owns both the registry and the
   `set_event_sink` call site.
