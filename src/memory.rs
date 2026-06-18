//! Run memory: the goal plus a typed transcript of what the agent planned and
//! observed. It is the single source of truth a planner reads to rebuild the
//! next model request (the API is stateless — memory is the state).

use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionOutcome};

/// One entry in the transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Record {
    /// The planner's turn: its reasoning and the actions it chose.
    Plan {
        step: usize,
        thought: Option<String>,
        actions: Vec<Action>,
    },
    /// The result of executing one action (tool result or a recovered error).
    /// `Finished` outcomes are not recorded — the run ends instead.
    Outcome { step: usize, outcome: ActionOutcome },
}

/// Mutable run memory.
#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    goal: String,
    records: Vec<Record>,
}

/// A serializable point-in-time copy of [`Memory`] for replay/debugging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub schema: u32,
    pub goal: String,
    pub records: Vec<Record>,
}

impl Memory {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            records: Vec::new(),
        }
    }

    pub fn goal(&self) -> &str {
        &self.goal
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    pub fn record_plan(&mut self, step: usize, thought: Option<String>, actions: Vec<Action>) {
        self.records.push(Record::Plan {
            step,
            thought,
            actions,
        });
    }

    pub fn record_outcome(&mut self, step: usize, outcome: ActionOutcome) {
        self.records.push(Record::Outcome { step, outcome });
    }

    pub fn snapshot(&self) -> MemorySnapshot {
        MemorySnapshot {
            schema: 1,
            goal: self.goal.clone(),
            records: self.records.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::RecoverableError;
    use serde_json::json;

    #[test]
    fn records_plan_then_outcome_in_order() {
        let mut m = Memory::new("ship it");
        m.record_plan(
            1,
            Some("call the tool".into()),
            vec![Action::CallTool {
                call_id: "c1".into(),
                name: "calculator".into(),
                input: json!({"expression": "1+1"}),
            }],
        );
        m.record_outcome(
            1,
            ActionOutcome::ToolResult {
                call_id: "c1".into(),
                name: "calculator".into(),
                output: json!({"result": 2.0}),
            },
        );
        assert_eq!(m.goal(), "ship it");
        assert_eq!(m.records().len(), 2);
        assert!(matches!(m.records()[0], Record::Plan { step: 1, .. }));
        assert!(matches!(m.records()[1], Record::Outcome { step: 1, .. }));
    }

    #[test]
    fn snapshot_roundtrips_through_json() {
        let mut m = Memory::new("g");
        m.record_outcome(
            2,
            ActionOutcome::Recoverable(RecoverableError::UnknownTool("nope".into())),
        );
        let snap = m.snapshot();
        assert_eq!(snap.schema, 1);
        let text = serde_json::to_string(&snap).unwrap();
        let back: MemorySnapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(snap, back);
    }
}
