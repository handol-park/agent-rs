//! Built-in tools. Kept deliberately small and self-contained — the framework's
//! weight belongs in the loop, not in a leaf tool.

use serde_json::{json, Value};

use super::Tool;
use crate::error::ToolError;

/// Evaluates a basic arithmetic expression: `+ - * /`, parentheses, and unary
/// `+`/`-`. Proves the JSON-in/JSON-out tool path end to end.
pub struct CalculatorTool;

impl Tool for CalculatorTool {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Evaluate a basic arithmetic expression (supports + - * /, parentheses)."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "An arithmetic expression, e.g. \"2 + 3 * (4 - 1)\""
                }
            },
            "required": ["expression"]
        })
    }

    fn execute(&self, input: &Value) -> Result<Value, ToolError> {
        let expression = input
            .get("expression")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidInput("expected string field 'expression'".into()))?;
        let value = eval(expression)?;
        Ok(json!({ "result": value }))
    }
}

/// Evaluate a full expression and require all input to be consumed.
fn eval(src: &str) -> Result<f64, ToolError> {
    let mut p = Parser {
        bytes: src.as_bytes(),
        pos: 0,
    };
    let value = p.expr()?;
    p.ws();
    if p.pos != p.bytes.len() {
        return Err(ToolError::InvalidInput("unexpected trailing input".into()));
    }
    Ok(value)
}

/// A tiny recursive-descent evaluator. Grammar:
/// `expr := term (('+'|'-') term)*`, `term := factor (('*'|'/') factor)*`,
/// `factor := ('-'|'+') factor | '(' expr ')' | number`.
struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.pos < self.bytes.len() && self.bytes[self.pos] == c {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Result<f64, ToolError> {
        let mut value = self.term()?;
        loop {
            self.ws();
            if self.eat(b'+') {
                value += self.term()?;
            } else if self.eat(b'-') {
                value -= self.term()?;
            } else {
                break;
            }
        }
        Ok(value)
    }

    fn term(&mut self) -> Result<f64, ToolError> {
        let mut value = self.factor()?;
        loop {
            self.ws();
            if self.eat(b'*') {
                value *= self.factor()?;
            } else if self.eat(b'/') {
                let divisor = self.factor()?;
                if divisor == 0.0 {
                    return Err(ToolError::ExecutionFailed("division by zero".into()));
                }
                value /= divisor;
            } else {
                break;
            }
        }
        Ok(value)
    }

    fn factor(&mut self) -> Result<f64, ToolError> {
        self.ws();
        if self.eat(b'-') {
            return Ok(-self.factor()?);
        }
        if self.eat(b'+') {
            return self.factor();
        }
        if self.eat(b'(') {
            let value = self.expr()?;
            self.ws();
            if !self.eat(b')') {
                return Err(ToolError::InvalidInput("missing ')'".into()));
            }
            return Ok(value);
        }
        self.number()
    }

    fn number(&mut self) -> Result<f64, ToolError> {
        self.ws();
        let start = self.pos;
        while self.pos < self.bytes.len()
            && (self.bytes[self.pos].is_ascii_digit() || self.bytes[self.pos] == b'.')
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(ToolError::InvalidInput("expected a number".into()));
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .expect("digits and '.' are valid ASCII");
        text.parse::<f64>()
            .map_err(|_| ToolError::InvalidInput("invalid number".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc(expr: &str) -> Result<Value, ToolError> {
        CalculatorTool.execute(&json!({ "expression": expr }))
    }

    #[test]
    fn respects_precedence_and_parens() {
        assert_eq!(calc("1 + 2 * 3").unwrap(), json!({"result": 7.0}));
        assert_eq!(calc("(1 + 2) * 3").unwrap(), json!({"result": 9.0}));
    }

    #[test]
    fn handles_unary_minus() {
        assert_eq!(calc("-3 + 5").unwrap(), json!({"result": 2.0}));
    }

    #[test]
    fn missing_field_is_invalid_input() {
        let err = CalculatorTool.execute(&json!({})).unwrap_err();
        assert_eq!(
            err,
            ToolError::InvalidInput("expected string field 'expression'".into())
        );
    }

    #[test]
    fn garbage_expression_is_invalid_input() {
        assert!(matches!(
            calc("1 +").unwrap_err(),
            ToolError::InvalidInput(_)
        ));
    }

    #[test]
    fn division_by_zero_is_execution_failure() {
        assert_eq!(
            calc("1 / 0").unwrap_err(),
            ToolError::ExecutionFailed("division by zero".into())
        );
    }
}
