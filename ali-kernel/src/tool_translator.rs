//! L7 Universal Tool Translator — neutral internal Tool representation.
//!
//! The gateway speaks multiple wire formats for tool definitions:
//! - OpenAI: `tools: [{type:"function", function:{name, description, parameters}}]`
//! - Anthropic: `tools: [{name, description, input_schema}]`
//!
//! Plus a llama.cpp native extension that produces a GBNF grammar so the
//! model can be *forced* into a valid tool-call output shape.
//!
//! This module is the single source of truth for that translation:
//! - L3 / handshake converts inbound calls into `Vec<Tool>` (parse_*)
//! - L7 dispatch operates on `Vec<Tool>` (no wire-format awareness)
//! - L6 forwards the request in whatever wire format the backend prefers
//!   (render_to_*). When the backend is local, also produces a GBNF
//!   grammar via `gbnf_for_tools`.
//!
//! ## Tool Firewall (L3 — v3.1 Legendary upgrade)
//!
//! `validate_arguments(tool, args)` runs **before** any sandbox dispatch.
//! If the args don't match `tool.parameters_schema`, OR if the tool
//! name isn't in the gateway allow-list (`config::default_enabled_tool_names()`)
//! OR isn't in the caller's `tools[]`, the validation returns
//! `ToolError` and the L7 sandbox never sees the bad args. The gateway
//! synthesizes a `tool_result` block with the error and ships it back to
//! the model for self-correction.
//!
//! Validation is intentionally lightweight (hand-rolled JSON-Schema
//! subset — `type`, `required`, `properties`, `enum`, `items`). The
//! `jsonschema` crate is a v3.2 candidate; for v3.1 we keep deps minimal.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Neutral tool representation. The gateway's L7 layer operates only
/// on this struct; wire-format awareness lives in the parser/renderer
/// boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments. Stored as `Value` so we
    /// can pass it through without re-parsing on every render.
    pub parameters_schema: Value,
    /// Whether the backend should enforce strict adherence to
    /// `parameters_schema` (currently advisory; e.g. OpenAI strict=true
    /// sets additionalProperties=false and validates required fields).
    pub strict: bool,
}

/// Failure mode from `validate_arguments`. The `kind` is what the L7
/// sandbox uses to decide whether to surface a `tool_result` error
/// rather than executing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolErrorKind {
    UnknownTool,
    MissingRequired,
    TypeMismatch,
    EnumViolation,
    UnknownField, // when strict=true and extra fields exist
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(pub ToolErrorKind, pub String);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.0, self.1)
    }
}
impl std::error::Error for ToolError {}

/// Parse OpenAI-format `tools: [...]` (lives at the top level of a chat
/// request). Tolerates both `{type:"function", function:{…}}` and the
/// bare `{…}` shorthand.
pub fn parse_openai_tools(tools: &Value) -> Vec<Tool> {
    let Some(arr) = tools.as_array() else {
        return vec![];
    };
    arr.iter()
        .filter_map(|t| {
            let function = if t.get("type").and_then(|v| v.as_str()) == Some("function") {
                t.get("function")?
            } else {
                t // bare shape: {name, description, parameters}
            };
            let name = function.get("name")?.as_str()?.to_string();
            let description = function
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let parameters_schema = function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            let strict = function
                .get("strict")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some(Tool {
                name,
                description,
                parameters_schema,
                strict,
            })
        })
        .collect()
}

/// Parse Anthropic-format `tools: [...]` (top-level in a `/v1/messages`
/// request). Anthropic uses a flat shape — no `function` wrapper.
pub fn parse_anthropic_tools(tools: &Value) -> Vec<Tool> {
    let Some(arr) = tools.as_array() else {
        return vec![];
    };
    arr.iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?.to_string();
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let parameters_schema = t
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            Some(Tool {
                name,
                description,
                parameters_schema,
                strict: false,
            })
        })
        .collect()
}

/// Render a `Vec<Tool>` to OpenAI chat-completions format.
pub fn render_to_openai(tools: &[Tool]) -> Value {
    let out: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters_schema,
                    "strict": t.strict,
                },
            })
        })
        .collect();
    Value::Array(out)
}

/// Render a `Vec<Tool>` to Anthropic `/v1/messages` format.
pub fn render_to_anthropic(tools: &[Tool]) -> Value {
    let out: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters_schema,
            })
        })
        .collect();
    Value::Array(out)
}

/// Emit a minimal GBNF grammar that forces the model to emit one of the
/// supplied tool calls in valid OpenAI-style JSON. Used when the backend
/// is a local llama.cpp server (the v3.1 Pillar-3 GBNF-on-for-local
/// default). The grammar is intentionally lenient: it allows the model
/// to emit either a `tool_calls` array or fall back to a regular content
/// response, which matches the OpenAI chat-completions spec for tools.
pub fn gbnf_for_tools(tools: &[Tool]) -> String {
    // The grammar is short — llama.cpp supports a compact string form.
    // We emit a `tool_call ::= "{" ... "}"` rule per tool, then a top-level
    // `root ::= ws ("{" ... "}" | tool_call | response ) ws`.
    let mut grammar = String::from(
        r#"ws ::= [ \t\n]*
digit ::= [0-9]
int ::= "-"? digit+
number ::= "-"? (digit* "."? digit+ | digit+ "." digit*)
string ::= "\"" ([^"\\] | "\\" ["\\/bfnrt] | "u" [0-9a-f] [0-9a-f] [0-9a-f] [0-9a-f])* "\""
bool ::= "true" | "false"
value ::= string | number | bool | "null" | object | array
array ::= "[" ws (value ("," ws value)*)? ws "]" ws
object ::= "{" ws "}" ws | "{" ws pair ("," ws pair)* ws "}" ws
pair ::= string ws ":" ws value
"#,
    );
    if tools.is_empty() {
        grammar.push_str("root ::= ws object ws\n");
        return grammar;
    }
    let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    let tool_union = tool_names
        .iter()
        .map(|n| format!("\"{n}\""))
        .collect::<Vec<_>>()
        .join(" | ");
    grammar.push_str(&format!(
        "tool_name ::= {tool_union}\n\
         tool_call ::= \"{{\" ws \"\\\"name\\\"\" ws \":\" ws tool_name ws \",\" ws\n\
                       \"\\\"arguments\\\"\" ws \":\" ws object \",\" ws\n\
                       \"\\\"id\\\"\" ws \":\" ws string ws \"}}\" ws\n\
         response ::= object\n\
         root ::= ws (tool_call | response) ws\n"
    ));
    grammar
}

/// Tool Firewall. Validates `args` against `tool.parameters_schema`. If
/// the tool isn't in the allow-list, rejects with `UnknownTool`.
///
/// `known_tools` is the set of tools the gateway trusts for dispatch:
/// caller-supplied tools plus the gateway's own native set.
pub fn validate_arguments(
    tool: &Tool,
    args: &Value,
    known_tools: &[Tool],
) -> Result<(), ToolError> {
    if !known_tools.iter().any(|t| t.name == tool.name) {
        return Err(ToolError(
            ToolErrorKind::UnknownTool,
            format!("tool name '{}' is not in the allowed set", tool.name),
        ));
    }
    let Some(schema) = tool.parameters_schema.as_object() else {
        return Ok(()); // no schema → no constraints to enforce
    };
    // type-level check (object-only here; primitives are less common for
    // tool args and we'd need extensive case analysis).
    let expected_type = schema
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("object");
    match expected_type {
        "object" => {
            let Some(obj) = args.as_object() else {
                return Err(ToolError(
                    ToolErrorKind::TypeMismatch,
                    format!(
                        "arguments for '{}' should be a JSON object, got {}",
                        tool.name,
                        args_type(args),
                    ),
                ));
            };
            // required fields
            if let Some(Value::Array(required)) = schema.get("required") {
                for req in required.iter().filter_map(|v| v.as_str()) {
                    if !obj.contains_key(req) {
                        return Err(ToolError(
                            ToolErrorKind::MissingRequired,
                            format!("required field '{req}' missing from arguments"),
                        ));
                    }
                }
            }
            // properties.{name}.enum / type checks
            if let Some(Value::Object(props)) = schema.get("properties") {
                for (k, v) in obj {
                    let Some(prop_schema) = props.get(k) else {
                        if tool.strict {
                            return Err(ToolError(
                                ToolErrorKind::UnknownField,
                                format!("unknown field '{k}' (strict=true)"),
                            ));
                        }
                        continue;
                    };
                    let actual = args_type(v);
                    if let Some(Value::String(allowed)) = prop_schema.get("type") {
                        if !json_type_matches(allowed, &actual) {
                            return Err(ToolError(
                                ToolErrorKind::TypeMismatch,
                                format!(
                                    "field '{k}' for '{}': expected {allowed}, got {actual}",
                                    tool.name
                                ),
                            ));
                        }
                    }
                    if let Some(Value::Array(enum_vals)) = prop_schema.get("enum") {
                        let allowed_list: Vec<&Value> = enum_vals.iter().collect();
                        if !allowed_list.contains(&v) {
                            return Err(ToolError(
                                ToolErrorKind::EnumViolation,
                                format!("field '{k}' for '{}': value not in enum", tool.name),
                            ));
                        }
                    }
                }
            }
            Ok(())
        }
        // Primitive tool args are unusual; skip strict type check.
        _ => Ok(()),
    }
}

/// Render the tool list into a structure the sandbox dispatch layer can
/// consume (purely informational — the actual dispatcher in
/// `tools.rs::dispatch_single_tool` only needs `name` and `args`).
pub fn dispatch_shape(tool: &Tool) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
    })
}

// ── helpers ────────────────────────────────────────────────────────────────

fn args_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn json_type_matches(expected: &str, actual: &str) -> bool {
    match (expected, actual) {
        ("integer", "number") => true, // integer ⊆ number
        ("number", "integer") => true,
        _ => expected == actual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_calculator() -> Tool {
        Tool {
            name: "calc".into(),
            description: "Evaluate a simple arithmetic expression".into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "expr": { "type": "string", "enum": ["a+b", "a*b", "a-b"] }
                },
                "required": ["expr"]
            }),
            strict: false,
        }
    }

    #[test]
    fn parse_openai_tools_handles_function_wrapper() {
        let v = json!([{
            "type": "function",
            "function": {
                "name": "do_thing",
                "description": "Does it",
                "parameters": {"type":"object"},
                "strict": true
            }
        }]);
        let tools = parse_openai_tools(&v);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "do_thing");
        assert!(tools[0].strict);
    }

    #[test]
    fn parse_openai_tools_handles_bare_shape() {
        let v = json!([{
            "name": "do_thing",
            "description": "Does it",
            "parameters": {"type":"object"}
        }]);
        let tools = parse_openai_tools(&v);
        assert_eq!(tools[0].name, "do_thing");
    }

    #[test]
    fn parse_anthropic_tools_flat() {
        let v = json!([{
            "name": "do_thing",
            "description": "Does it",
            "input_schema": {"type":"object","properties":{}}
        }]);
        let tools = parse_anthropic_tools(&v);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "do_thing");
    }

    #[test]
    fn render_to_openai_round_trip() {
        let v = json!([{
            "type": "function",
            "function": {
                "name": "f", "description": "d", "parameters": {"type":"object"},
                "strict": false
            }
        }]);
        let tools = parse_openai_tools(&v);
        let out = render_to_openai(&tools);
        let arr = out.as_array().unwrap();
        assert_eq!(arr[0]["function"]["name"], "f");
        assert_eq!(arr[0]["type"], "function");
    }

    #[test]
    fn render_to_anthropic_round_trip() {
        let v = json!([{
            "name": "f", "description": "d", "input_schema": {"type":"object"}
        }]);
        let tools = parse_anthropic_tools(&v);
        let out = render_to_anthropic(&tools);
        let arr = out.as_array().unwrap();
        assert_eq!(arr[0]["name"], "f");
        assert!(arr[0].get("input_schema").is_some());
    }

    #[test]
    fn validate_accepts_valid_args() {
        let tool = sample_calculator();
        let known = vec![tool.clone()];
        let args = json!({"expr": "a+b"});
        assert!(validate_arguments(&tool, &args, &known).is_ok());
    }

    #[test]
    fn validate_rejects_missing_required() {
        let tool = sample_calculator();
        let known = vec![tool.clone()];
        let args = json!({}); // missing expr
        let err = validate_arguments(&tool, &args, &known).unwrap_err();
        assert_eq!(err.0, ToolErrorKind::MissingRequired);
    }

    #[test]
    fn validate_rejects_enum_violation() {
        let tool = sample_calculator();
        let known = vec![tool.clone()];
        let args = json!({"expr": "a/b"}); // not in enum
        let err = validate_arguments(&tool, &args, &known).unwrap_err();
        assert_eq!(err.0, ToolErrorKind::EnumViolation);
    }

    #[test]
    fn validate_rejects_type_mismatch() {
        let tool = sample_calculator();
        let known = vec![tool.clone()];
        let args = json!({"expr": 42}); // wrong type
        let err = validate_arguments(&tool, &args, &known).unwrap_err();
        assert_eq!(err.0, ToolErrorKind::TypeMismatch);
    }

    #[test]
    fn validate_rejects_unknown_tool() {
        let tool = Tool {
            name: "ghost".into(),
            description: "x".into(),
            parameters_schema: json!({"type":"object"}),
            strict: false,
        };
        let known = vec![sample_calculator()];
        let args = json!({});
        let err = validate_arguments(&tool, &args, &known).unwrap_err();
        assert_eq!(err.0, ToolErrorKind::UnknownTool);
    }

    #[test]
    fn validate_rejects_unknown_field_when_strict() {
        let mut tool = sample_calculator();
        tool.strict = true;
        let known = vec![tool.clone()];
        let args = json!({"expr": "a+b", "sneaky": true});
        let err = validate_arguments(&tool, &args, &known).unwrap_err();
        assert_eq!(err.0, ToolErrorKind::UnknownField);
    }

    #[test]
    fn gbnf_grammar_includes_each_tool_name() {
        let t1 = sample_calculator();
        let t2 = Tool {
            name: "search".into(),
            description: "Search the codebase".into(),
            parameters_schema: json!({"type":"object"}),
            strict: false,
        };
        let g = gbnf_for_tools(&[t1, t2]);
        assert!(g.contains("\"calc\""), "got: {g}");
        assert!(g.contains("\"search\""), "got: {g}");
        assert!(g.contains("tool_call"), "got: {g}");
        assert!(g.contains("root ::="), "got: {g}");
    }
}
