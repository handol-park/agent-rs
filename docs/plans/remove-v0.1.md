# Implementation Plan: Remove v0.1 Generation

**Scope**: Remove the v0.1 `Agent::run` loop generation entirely while keeping shared infrastructure (Provider, ToolRegistry) and v0.2 (Mind + Brainstem).

**Based on**: Issue #13

## Verification Phase (P0)

Before deleting, confirm usage of ambiguous symbols:

1. **`Action` / `RecoverableError`** (`src/action.rs`):
   - Grep for v0.2 usage
   - `RecoverableError` is likely used by v0.2 `RecoverableObservation` event → keep
   - Decision: keep or delete per-symbol

2. **`Memory` / `Record` / `ActionOutcome`** (`src/memory.rs`):
   - Grep for v0.2 usage (Mind, Brainstem, Observation, etc.)
   - If unused by v0.2 → delete
   - If used → keep

**Deliverable**: List of symbols to delete vs. keep

## Deletion Phase (P1 - depends on P0)

Delete v0.1-exclusive code in this order:

### 1. Examples & Tests
- Delete `examples/run.rs`
- Delete `tests/loop_recovery.rs`
- Remove `[[example]]` entry for `run` from `Cargo.toml`

### 2. Planner Module
- Delete entire `src/planner/` directory (mod.rs, model.rs, rule.rs)

### 3. Core v0.1 Loop
- Delete from `src/lib.rs`:
  - `Agent` struct
  - `Agent::run` / `run_with_events`
  - `RunReport`
  - All v0.1 unit tests in the file
  - v0.1-only crate-root re-exports: `Budget`, `TerminalReason`, `Planner`, `ModelPlanner`, `RulePlanner`, `PlanContext`, `PlanOutput`, `Memory`, `MemorySnapshot`, `Record`, `ActionOutcome`

### 4. Budget Types
- Delete from `src/budget.rs`:
  - v0.1 `Budget` struct
  - v0.1 `TerminalReason` enum
- **KEEP**: v0.2 `Period`, `RenewableBudget`, `BudgetState`, `BudgetSummary`

### 5. Error Types
- Delete from `src/error.rs`:
  - `PlannerError`
- **KEEP**: `ProviderError`, `ErrorClass`, `ToolError`, `AgentError`

### 6. Event Types
- Delete from `src/event.rs`:
  - v0.1-only `RunEvent` variants: `StepStarted`, `Planned`, `ToolCalled`, `ToolSucceeded`, `Recovered`, `Finished`
- **KEEP**: v0.2 variants (`TaskReceived`, `Command`, `CommandResult`, `RetryScheduled`, `WindowReset`, `ThrottleSleep`, `TaskCompleted`, `TaskFailed`, `Termination`)

### 7. Conditional Deletion (based on P0)
- `src/action.rs`: delete if unused by v0.2
- `src/memory.rs`: delete if unused by v0.2

## Documentation Update Phase (P2 - depends on P1)

### 1. `CLAUDE.md`
- Rewrite Architecture section: v0.2 only
- Add one-line historical note about v0.1 removal

### 2. `README.md`
- Update Status section
- Update entry-point references: `examples/service.rs` (not `examples/run.rs`)

## Validation Phase (P3 - depends on P2)

Run full verification suite:

```bash
nix develop -c make check    # fmt + clippy -D warnings + test
nix develop -c cargo build   # check for dead-code/unused-import warnings
nix develop -c cargo run --example service  # smoke test v0.2 example
```

If available, run v0.2 e2e test:
```bash
nix develop -c cargo test --test e2e_ollama  # requires local Ollama
```

## Acceptance Criteria

- ✅ `nix develop -c make check` passes
- ✅ No `Agent::run` / `Planner` / v0.1 `Budget` symbols remain
- ✅ No dead-code or unused-import warnings
- ✅ `examples/service.rs` builds and runs
- ✅ `tests/e2e_ollama.rs` passes (if Ollama available)
- ✅ Docs describe v0.2 as the sole generation

## Out of Scope

- `docs/specs/001-agent-core.md` and `docs/plans/001-agent-core.md` kept as historical record
