# Plan: Wire tool schemas into ModelMind

Spec: `docs/specs/003-modelmind-tool-schemas.md`
Branch: `feat/003-modelmind-tool-schemas`
Issue: #8

## Components

The change is a single thin wire from the brainstem-owned registry to the
provider request, plus tests. All three production edits are small and sit on an
existing seam (the `set_event_sink` injection precedent).

| Component | Files | Dependencies |
|-----------|-------|--------------|
| C1 — `Mind::set_tools` trait hook | `src/mind/mod.rs` | none |
| C2 — `ModelMind` stores + sends schemas | `src/mind/model.rs` | C1 |
| C3 — Brainstem injects `registry.schemas()` | `src/brainstem/mod.rs` | C1 |
| C4 — Unit test: schemas reach the request | `src/mind/model.rs` (`#[cfg(test)]`) | C2 |
| C5 — Integration test: end-to-end tool-call loop + advertised schemas | `tests/model_tool_schemas.rs` (new) | C2, C3 |

C1 must land first (C2 and C3 both depend on the trait method). C2 and C3 are
then independent of each other. C4 depends on C2; C5 depends on both C2 and C3.

## Contracts

The only new surface is one trait method. Everything else
(`ToolSchema`, `ToolRegistry::schemas()`, `ModelRequest.tools`) already exists
and is unchanged.

```rust
// src/mind/mod.rs — new default method on the Mind trait, mirroring set_event_sink.
#[async_trait]
pub trait Mind: Send + Sync {
    // ... existing methods ...

    /// Inject the tool schemas the brainstem's registry advertises, so a
    /// model-backed mind can tell the provider which tools exist. Default is a
    /// no-op for minds that don't talk to a provider (e.g. `FakeMind`).
    fn set_tools(&mut self, _tools: Vec<ToolSchema>) {}
}

// src/mind/model.rs — ModelMind gains an owned snapshot field.
pub struct ModelMind {
    // ... existing fields ...
    tools: Vec<ToolSchema>,   // empty until set_tools is called; cloned into every request
}
```

Injection timing (informational): the brainstem calls `set_tools` **once**, at
`run()` start, right after the existing `set_event_sink`. The mind keeps its own
`Vec<ToolSchema>` for the life of the run (snapshot, not a live view). Dynamic /
refreshable tool sets are explicitly out of scope — tracked separately in
issue #9.

## Changes

### New files

- `tests/model_tool_schemas.rs` — spec-003 acceptance: a small brainstem harness
  (mirroring `tests/actor_loop.rs`) driving a real `ModelMind` against a
  `FakeProvider` scripted to return a native `calculator` tool call, then a final
  text answer. Asserts SC-4 end-to-end.

### Modified files

- `src/mind/mod.rs` — add `use crate::tool::ToolSchema;` and the `set_tools`
  default method on the `Mind` trait (C1, SC-1).
- `src/mind/model.rs`:
  - add `use crate::tool::ToolSchema;`
  - add `tools: Vec<ToolSchema>` field, initialized to `Vec::new()` in `new()`
    (C2, SC-2: never-injected → empty vec).
  - implement `fn set_tools(&mut self, tools: Vec<ToolSchema>) { self.tools = tools; }`
    in the `impl Mind for ModelMind` block.
  - in `call_with_retry`, replace `tools: Vec::new(), // TODO: pass tool schemas`
    with `tools: self.tools.clone()` (C2, SC-2: included in every request,
    initial + retries — the assignment is inside the retry `loop`).
  - add a `#[cfg(test)]` unit test (C4) asserting that after `set_tools`, the
    request recorded by `FakeProvider::requests_handle()` carries the schemas.
- `src/brainstem/mod.rs` — in `run()`, immediately after the existing
  `set_event_sink` call, add `self.mind.set_tools(self.registry.schemas());`
  (C3, SC-3). `self.registry` is `Arc<ToolRegistry>`; `schemas()` takes `&self`,
  so no ownership change.

### Deleted files

- none

## Test Strategy

Tests are written against the success criteria and must fail before the wire is
added (the unit test would see an empty `tools` vec; the integration test's
schema assertion would fail). All time-independent here — no `start_paused`
needed except to match the existing harness style.

- **Unit tests** (`src/mind/model.rs`, C4 → SC-2):
  - `set_tools_schemas_reach_the_provider_request`: build `ModelMind` with a
    `FakeProvider` (grab `requests_handle()` first), call
    `set_tools(default_registry().schemas())`, drive one `decide(NewTask)` that
    the provider answers with text. Assert the recorded `request.tools` is
    non-empty and contains a schema named `"calculator"`.
  - `without_set_tools_request_advertises_no_tools` (SC-2 negative): same setup
    minus `set_tools`; assert the recorded `request.tools` is empty (no panic).
- **Integration test** (`tests/model_tool_schemas.rs`, C5 → SC-4):
  - Scripted `FakeProvider`:
    1. `ModelResponse::tool_call("c1", "calculator", {"expression": "1+1"})`
    2. `ModelResponse::text("the answer is 2")`
  - Grab `requests_handle()`, build `ModelMind`, spawn a `Brainstem` with
    `Arc::new(default_registry())` (has the calculator).
  - Send one `Task`; await its `TaskOutcome`. Assert `Completed` — proving the
    tool call was actuated through the registry (calculator → `2.0`), folded
    back as an `Observation::ToolResult`, and the next turn returned `Done`.
  - Assert the **first** recorded request advertised the calculator schema
    (proving the brainstem injected `registry.schemas()` and it reached the
    wire — the byte the bug was missing).
  - Optionally assert the second request carries the tool result message,
    confirming the observation folded back (belt-and-suspenders for the loop).
- **Acceptance (from spec Success Criteria)**:
  - SC-1 → `FakeMind` still compiles unchanged (no `set_tools` override needed);
    confirmed by the existing `tests/actor_loop.rs` suite continuing to pass.
  - SC-2 → both unit tests above.
  - SC-3 → the integration test's "first request advertised the schema" assert.
  - SC-4 → the integration test end-to-end.
  - SC-5 → `nix develop -c make check` (fmt + clippy `-D warnings` + test).

## Open Questions

1. Integration-test placement: a **new** `tests/model_tool_schemas.rs` (keeps
   spec-003 acceptance isolated and matches the one-spec-per-file convention) vs.
   appending to `tests/actor_loop.rs` (reuses its private harness). **Recommended
   (informational): new file** with a minimal local harness copied from
   `actor_loop.rs`; the duplication is a few helper fns and keeps the spec-002
   file from accreting spec-003 concerns.
2. Should the unit test assert the schema *contents* (e.g. the `parameters`
   JSON Schema), or only that the named tool is present? **Recommended: assert
   presence by name** — schema-content correctness is already covered by
   `tool/mod.rs`'s `schemas_are_sorted_and_describe_tools` test; this spec only
   needs to prove the schemas were *carried through*, not re-test their shape.
