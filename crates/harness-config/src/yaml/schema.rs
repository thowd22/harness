//! JSON Schema (draft 2020-12) for editor completion: `harness schema`.
//!
//! Point an editor's YAML language server at the output, e.g. for
//! yaml-language-server add `# yaml-language-server: $schema=agent.json` at
//! the top of a file.

use std::io::Write;

use serde_json::{Value, json};

use crate::agents::Agent;
use crate::prompts::Prompt;

/// Which schema to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaKind {
    /// `prompts/*.yaml`.
    Prompt,
    /// `agents/*.yaml`.
    Agent,
    /// `workflows/*.yaml`.
    Workflow,
}

impl SchemaKind {
    /// All kinds, in output order.
    pub const ALL: [SchemaKind; 3] = [SchemaKind::Prompt, SchemaKind::Agent, SchemaKind::Workflow];

    /// CLI name (`prompt`, `agent`, `workflow`).
    pub fn name(self) -> &'static str {
        match self {
            SchemaKind::Prompt => "prompt",
            SchemaKind::Agent => "agent",
            SchemaKind::Workflow => "workflow",
        }
    }

    /// Parses a CLI name; plural forms are accepted.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_suffix('s').unwrap_or(s);
        Self::ALL.into_iter().find(|k| k.name() == s)
    }
}

/// The JSON Schema for one kind.
pub fn schema(kind: SchemaKind) -> Value {
    match kind {
        SchemaKind::Prompt => Prompt::json_schema(),
        SchemaKind::Agent => Agent::json_schema(),
        SchemaKind::Workflow => workflow_schema(),
    }
}

/// Entry point for `harness schema [prompt|agent|workflow]`.
///
/// With a kind, prints that schema; without, prints an object keyed by kind
/// name. Output is pretty JSON plus a newline. Returns the exit code: 0, or
/// 2 for an unknown kind (usage error).
pub fn run_schema(kind: Option<&str>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let value = match kind {
        None => Value::Object(
            SchemaKind::ALL
                .into_iter()
                .map(|k| (k.name().to_owned(), schema(k)))
                .collect(),
        ),
        Some(k) => match SchemaKind::parse(k) {
            Some(k) => schema(k),
            None => {
                let _ = writeln!(
                    err,
                    "harness schema: unknown kind `{k}`; expected prompt, agent or workflow"
                );
                return 2;
            }
        },
    };
    let text = serde_json::to_string_pretty(&value).unwrap_or_default();
    match writeln!(out, "{text}") {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(err, "harness schema: {e}");
            1
        }
    }
}

/// Workflow schema per the YAML spec (doc-3). harness-workflow owns full
/// workflow validation; this schema is for editor completion.
fn workflow_schema() -> Value {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    let str_or_list = json!({ "oneOf": [ { "type": "string" }, strings ] });
    let budget = json!({
        "type": "object", "additionalProperties": false,
        "properties": {
            "usd": { "type": "number", "minimum": 0 },
            "tokens": { "oneOf": [ { "type": "integer", "minimum": 0 }, { "type": "string", "pattern": "^[0-9][0-9_]*$" } ] }
        }
    });
    let runs_on = json!({
        "oneOf": [
            { "type": "string" },
            { "type": "object", "additionalProperties": false,
              "properties": { "labels": strings, "min_mem_gb": { "type": "integer", "minimum": 0 } } }
        ]
    });
    let step = json!({
        "type": "object", "additionalProperties": false, "required": ["id"],
        "properties": {
            "id": { "type": "string" },
            "type": { "enum": ["agent", "shell", "verify", "bugfix-proof", "approval", "workflow", "dream"] },
            "needs": strings,
            "if": { "type": "string" },
            "for_each": {},
            "matrix": { "type": "object" },
            "loop": {
                "type": "object", "additionalProperties": false, "required": ["until"],
                "properties": {
                    "until": { "type": "string" },
                    "max_iterations": { "type": "integer", "minimum": 1 },
                    "retry_step": { "type": "string" }
                }
            },
            "retries": { "type": "integer", "minimum": 0 },
            "timeout": { "oneOf": [ { "type": "integer", "minimum": 0 }, { "type": "string" } ] },
            "on_failure": { "enum": ["abort", "continue"] },
            "runs_on": runs_on,
            "scope": str_or_list,
            "budget": budget,
            "outputs": strings,
            "artifacts": strings,
            "agent": { "type": "string" },
            "model": { "type": "string" },
            "prompt": { "type": "string" },
            "run": { "type": "string" },
            "verify": {
                "oneOf": [
                    { "type": "string" },
                    { "type": "object", "additionalProperties": false, "required": ["command"],
                      "properties": { "command": { "type": "string" }, "expect": { "enum": ["pass", "fail"] } } }
                ]
            },
            "test": { "type": "string" },
            "message": { "type": "string" },
            "uses": { "type": "string" },
            "with": { "type": "object" }
        }
    });
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://harness.dev/schema/workflow.json",
        "title": "harness workflow",
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "steps"],
        "properties": {
            "name": { "type": "string", "minLength": 1 },
            "description": { "type": "string" },
            "command": { "type": "string", "description": "Slash command name (defaults to name)." },
            "inputs": {
                "type": "object",
                "additionalProperties": {
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "type": { "enum": ["string", "list", "number", "bool"] },
                        "required": { "type": "boolean" },
                        "default": {},
                        "positional": { "type": "integer", "minimum": 0 },
                        "description": { "type": "string" }
                    }
                }
            },
            "budget": budget,
            "goal": { "type": "string", "description": "Shown at the top of the goal bar." },
            "memory": {
                "enum": ["off", "read", "read-write"],
                "description": "The most memory access agent steps get; `off` turns memory off for the workflow."
            },
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object", "additionalProperties": false, "required": ["id", "title"],
                    "properties": { "id": { "type": "string" }, "title": { "type": "string" }, "step": { "type": "string" } }
                }
            },
            "steps": {
                "type": "array",
                "minItems": 1,
                "items": step
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(v: &Value) -> Vec<String> {
        v["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn prints_each_kind_and_all() {
        for k in ["prompt", "agents", "workflow"] {
            let mut out = Vec::new();
            assert_eq!(run_schema(Some(k), &mut out, &mut Vec::new()), 0);
            let v: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(v["$schema"], "https://json-schema.org/draft/2020-12/schema");
            assert_eq!(v["additionalProperties"], false);
        }
        let mut out = Vec::new();
        assert_eq!(run_schema(None, &mut out, &mut Vec::new()), 0);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 3);
        let mut err = Vec::new();
        assert_eq!(run_schema(Some("nope"), &mut Vec::new(), &mut err), 2);
        assert!(
            String::from_utf8(err)
                .unwrap()
                .contains("unknown kind `nope`")
        );
    }

    #[test]
    fn schemas_cover_every_spec_field() {
        let agent = props(&schema(SchemaKind::Agent));
        for f in [
            "name",
            "description",
            "model",
            "effort",
            "prompt",
            "prompt_text",
            "tools",
            "subagents",
            "memory",
            "mode",
            "max_turns",
            "budget",
            "scope",
            "policy",
            "verify",
            "runs_on",
            "output_schema",
        ] {
            assert!(agent.contains(&f.to_owned()), "agent schema lacks {f}");
        }
        assert_eq!(
            props(&schema(SchemaKind::Prompt)),
            ["description", "layers", "name", "vars"]
        );
        let wf = schema(SchemaKind::Workflow);
        for f in [
            "name",
            "description",
            "command",
            "inputs",
            "budget",
            "goal",
            "tasks",
            "steps",
        ] {
            assert!(
                props(&wf).contains(&f.to_owned()),
                "workflow schema lacks {f}"
            );
        }
        let step = &wf["properties"]["steps"]["items"];
        for f in [
            "id",
            "needs",
            "if",
            "for_each",
            "matrix",
            "loop",
            "retries",
            "timeout",
            "on_failure",
            "runs_on",
            "scope",
            "budget",
            "outputs",
            "artifacts",
            "type",
            "agent",
            "run",
            "verify",
            "uses",
        ] {
            assert!(props(step).contains(&f.to_owned()), "step schema lacks {f}");
        }
    }
}
