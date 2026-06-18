//! The tool interface and registry. Tools are **synchronous** — they take JSON
//! in and return JSON out. The core loop never branches on a specific tool; it
//! looks one up here and dispatches.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ToolError;

pub mod builtins;

/// A synchronous, JSON-in/JSON-out capability the agent can invoke.
pub trait Tool: Send + Sync {
    /// Unique, stable name the model calls.
    fn name(&self) -> &str;
    /// One-line description shown to the model.
    fn description(&self) -> &str;
    /// JSON Schema for this tool's input object (the `parameters` of a native
    /// tool-call function definition).
    fn parameters(&self) -> Value;
    /// Run the tool. Errors here are **recoverable** at the loop level.
    fn execute(&self, input: &Value) -> Result<Value, ToolError>;
}

/// A tool's advertised contract, handed to the planner so the model knows what
/// it can call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Returned when registering two tools with the same name — a construction-time
/// bug, distinct from a recoverable [`ToolError`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a tool named '{0}' is already registered")]
pub struct DuplicateTool(pub String);

/// A name-keyed set of tools. The loop's only tool-dispatch surface.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Fails if its name is already taken.
    pub fn register(&mut self, tool: Box<dyn Tool>) -> Result<(), DuplicateTool> {
        let name = tool.name().to_string();
        if self.tools.contains_key(&name) {
            return Err(DuplicateTool(name));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    /// Look up a tool by name. The loop uses this to distinguish an unknown tool
    /// (→ `RecoverableError::UnknownTool`) from a tool that ran and failed.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    /// Every tool's schema, sorted by name for deterministic output.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas: Vec<ToolSchema> = self
            .tools
            .values()
            .map(|t| ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect();
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        schemas
    }
}

/// A registry wired with the built-in tools.
pub fn default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry
        .register(Box::new(builtins::CalculatorTool))
        .expect("calculator registers into an empty registry");
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_registry_has_calculator() {
        let r = default_registry();
        assert!(r.get("calculator").is_some());
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut r = default_registry();
        let err = r
            .register(Box::new(builtins::CalculatorTool))
            .expect_err("second calculator should be rejected");
        assert_eq!(err, DuplicateTool("calculator".into()));
    }

    #[test]
    fn schemas_are_sorted_and_describe_tools() {
        let schemas = default_registry().schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "calculator");
        assert_eq!(schemas[0].parameters["required"], json!(["expression"]));
    }

    #[test]
    fn get_then_execute_runs_the_tool() {
        let r = default_registry();
        let out = r
            .get("calculator")
            .unwrap()
            .execute(&json!({"expression": "1 + 2 * 3"}))
            .unwrap();
        assert_eq!(out, json!({"result": 7.0}));
    }
}
