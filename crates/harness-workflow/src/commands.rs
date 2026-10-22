//! Workflows as slash commands.
//!
//! Every workflow registers `/<command>` (the `command:` key, default its
//! name). Arguments map onto inputs:
//!
//! - positional words fill inputs with `positional: N` in order; extra
//!   words are joined into the last positional string input (so
//!   `/fix header parsing breaks on CRLF` needs no quotes) or appended to a
//!   last positional list;
//! - `--name value`, `--name=value` and `name=value` set any input; a bare
//!   `--flag` sets a bool input to true; list inputs take comma-separated
//!   values or repeated flags;
//! - values are converted to the input's type (string, list, number,
//!   bool).
//!
//! Required inputs that are still missing are returned in
//! [`ParsedArgs::missing`] with a prompt each ([`CommandArg::prompt`]), so
//! the palette can ask for them and then call [`complete`]. [`help`] renders
//! usage and input documentation from the workflow description and inputs.

use harness_config::skills::split_args;
use serde_json::{Value, json};

use crate::executor::resolve_inputs;
use crate::schema::{Input, InputType, Workflow};

/// One argument of a workflow command.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandArg {
    /// Input name.
    pub name: String,
    /// Type.
    pub kind: InputType,
    /// Must be given.
    pub required: bool,
    /// Position, if positional.
    pub positional: Option<usize>,
    /// Default value.
    pub default: Option<Value>,
    /// Help text.
    pub description: Option<String>,
}

impl CommandArg {
    fn from_input(i: &Input) -> Self {
        Self {
            name: i.name.clone(),
            kind: i.kind,
            required: i.required,
            positional: i.positional,
            default: i.default.clone(),
            description: i.description.clone(),
        }
    }

    /// The prompt shown when the palette asks for this input.
    pub fn prompt(&self) -> String {
        let mut s = self.name.clone();
        if let Some(d) = &self.description {
            s.push_str(&format!(" ({d})"));
        }
        s
    }

    /// A placeholder hint for the input field.
    pub fn hint(&self) -> String {
        match self.kind {
            InputType::String => "text".into(),
            InputType::List => "comma-separated values".into(),
            InputType::Number => "number".into(),
            InputType::Bool => "true or false".into(),
        }
    }

    fn usage(&self) -> String {
        let core = match self.positional {
            Some(_) => format!("<{}>", self.name),
            None if self.kind == InputType::Bool => format!("--{}", self.name),
            None => format!("--{} <{}>", self.name, self.kind.as_str()),
        };
        if self.required {
            core
        } else {
            format!("[{core}]")
        }
    }
}

/// A workflow as a slash command.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowCommand {
    /// Command name, without `/`.
    pub name: String,
    /// Workflow name.
    pub workflow: String,
    /// One-line description.
    pub description: String,
    /// Arguments: positional first, then named.
    pub args: Vec<CommandArg>,
}

impl WorkflowCommand {
    /// `/fix <issue> [--scope <list>]`.
    pub fn usage(&self) -> String {
        let mut s = format!("/{}", self.name);
        for a in &self.args {
            s.push(' ');
            s.push_str(&a.usage());
        }
        s
    }
}

/// The command of `wf`.
pub fn command(wf: &Workflow) -> WorkflowCommand {
    WorkflowCommand {
        name: wf.command.clone(),
        workflow: wf.name.clone(),
        description: wf
            .description
            .clone()
            .unwrap_or_else(|| format!("Run workflow {}", wf.name)),
        args: wf.inputs.iter().map(CommandArg::from_input).collect(),
    }
}

/// Commands for `workflows`, sorted by name.
pub fn commands<'a>(workflows: impl IntoIterator<Item = &'a Workflow>) -> Vec<WorkflowCommand> {
    let mut v: Vec<WorkflowCommand> = workflows.into_iter().map(command).collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// Help text generated from the description and inputs.
pub fn help(wf: &Workflow) -> String {
    let cmd = command(wf);
    let mut s = format!("{}\n\nUsage: {}\n", cmd.description, cmd.usage());
    if !cmd.args.is_empty() {
        s.push_str("\nInputs:\n");
        let width = cmd.args.iter().map(|a| a.name.len()).max().unwrap_or(0);
        for a in &cmd.args {
            let mut line = format!("  {:width$}  {}", a.name, a.kind.as_str());
            if a.required {
                line.push_str(", required");
            }
            if let Some(p) = a.positional {
                line.push_str(&format!(", position {}", p + 1));
            }
            if let Some(d) = &a.default {
                line.push_str(&format!(", default {d}"));
            }
            if let Some(d) = &a.description {
                line.push_str(&format!(". {d}"));
            }
            s.push_str(line.trim_end());
            s.push('\n');
        }
    }
    if !wf.steps.is_empty() {
        let ids: Vec<&str> = wf.topo_order().map(|s| s.id.as_str()).collect();
        s.push_str(&format!("\nSteps: {}\n", ids.join(" -> ")));
    }
    s
}

/// Arguments parsed from a command line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedArgs {
    /// Inputs given (typed).
    pub inputs: serde_json::Map<String, Value>,
    /// Required inputs still missing (prompt for these).
    pub missing: Vec<CommandArg>,
}

impl ParsedArgs {
    /// True when nothing is missing.
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// Converts `text` to the input's type.
pub fn convert(kind: InputType, name: &str, text: &str) -> Result<Value, String> {
    match kind {
        InputType::String => Ok(json!(text)),
        InputType::List => Ok(Value::Array(
            text.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| json!(s))
                .collect(),
        )),
        InputType::Number => text
            .trim()
            .parse::<i64>()
            .map(|n| json!(n))
            .or_else(|_| text.trim().parse::<f64>().map(|f| json!(f)))
            .map_err(|_| format!("`{name}` must be a number, got `{text}`")),
        InputType::Bool => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" | "on" => Ok(json!(true)),
            "false" | "no" | "0" | "off" => Ok(json!(false)),
            _ => Err(format!("`{name}` must be true or false, got `{text}`")),
        },
    }
}

fn set(
    inputs: &mut serde_json::Map<String, Value>,
    input: &Input,
    text: &str,
) -> Result<(), String> {
    let v = convert(input.kind, &input.name, text)?;
    match (input.kind, inputs.get_mut(&input.name)) {
        (InputType::List, Some(Value::Array(existing))) => {
            if let Value::Array(more) = v {
                existing.extend(more);
            }
        }
        _ => {
            inputs.insert(input.name.clone(), v);
        }
    }
    Ok(())
}

/// Parses a slash-command argument string for `wf`.
pub fn parse_args(wf: &Workflow, args: &str) -> Result<ParsedArgs, String> {
    let words = split_args(args);
    let mut inputs = serde_json::Map::new();
    let mut positional: Vec<String> = Vec::new();
    let lookup = |name: &str| {
        wf.input(name).ok_or_else(|| {
            let known: Vec<&str> = wf.inputs.iter().map(|i| i.name.as_str()).collect();
            format!(
                "unknown input `{name}` for /{} (inputs: {})",
                wf.command,
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join(", ")
                }
            )
        })
    };
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        if let Some(flag) = w.strip_prefix("--").filter(|f| !f.is_empty()) {
            let (name, value) = match flag.split_once('=') {
                Some((n, v)) => (n.replace('-', "_"), Some(v.to_owned())),
                None => (flag.replace('-', "_"), None),
            };
            let input = lookup(&name)?;
            let value = match value {
                Some(v) => v,
                None if input.kind == InputType::Bool
                    && words
                        .get(i + 1)
                        .is_none_or(|n| convert(InputType::Bool, &name, n).is_err()) =>
                {
                    "true".to_owned()
                }
                None => {
                    i += 1;
                    words
                        .get(i)
                        .cloned()
                        .ok_or_else(|| format!("--{name} needs a value"))?
                }
            };
            set(&mut inputs, input, &value)?;
        } else if let Some((n, v)) = w.split_once('=')
            && let Some(input) = wf.input(n)
        {
            set(&mut inputs, input, v)?;
        } else {
            positional.push(w.clone());
        }
        i += 1;
    }
    let slots: Vec<&Input> = {
        let mut v: Vec<&Input> = wf
            .inputs
            .iter()
            .filter(|i| i.positional.is_some() && !inputs.contains_key(&i.name))
            .collect();
        v.sort_by_key(|i| i.positional);
        v
    };
    if !positional.is_empty() {
        if slots.is_empty() {
            return Err(format!(
                "/{} takes no positional arguments; use --name value (inputs: {})",
                wf.command,
                wf.inputs
                    .iter()
                    .map(|i| i.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for (k, slot) in slots.iter().enumerate() {
            if k >= positional.len() {
                break;
            }
            let last = k == slots.len() - 1;
            if last && positional.len() > slots.len() {
                let rest = &positional[k..];
                match slot.kind {
                    InputType::String => set(&mut inputs, slot, &rest.join(" "))?,
                    InputType::List => set(&mut inputs, slot, &rest.join(","))?,
                    _ => {
                        return Err(format!(
                            "too many arguments for /{}: {}",
                            wf.command,
                            rest[1..].join(" ")
                        ));
                    }
                }
            } else {
                set(&mut inputs, slot, &positional[k])?;
            }
        }
    }
    let missing = wf
        .inputs
        .iter()
        .filter(|i| i.required && !inputs.contains_key(&i.name))
        .map(CommandArg::from_input)
        .collect();
    Ok(ParsedArgs { inputs, missing })
}

/// Adds the palette's answers for missing inputs and returns the complete,
/// validated inputs (defaults filled in).
pub fn complete(
    wf: &Workflow,
    parsed: &ParsedArgs,
    answers: &[(String, String)],
) -> Result<serde_json::Map<String, Value>, String> {
    let mut inputs = parsed.inputs.clone();
    for (name, text) in answers {
        let input = wf
            .input(name)
            .ok_or_else(|| format!("unknown input `{name}`"))?;
        if text.trim().is_empty() && input.required {
            return Err(format!("`{name}` is required"));
        }
        set(&mut inputs, input, text)?;
    }
    resolve_inputs(wf, &inputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse;

    const WF: &str = r#"name: fix-bug
description: Reproduce, fix and prove a bug fix
command: fix
inputs:
  issue: { type: string, required: true, positional: 0, description: "The bug" }
  scope: { type: list, default: ["src/**"] }
  max: { type: number }
  dry: { type: bool }
steps:
  - id: a
    run: "echo {{ inputs.issue }}"
"#;

    fn wf() -> Workflow {
        parse(WF, None).unwrap()
    }

    #[test]
    fn registers_the_command_with_usage_and_help() {
        let w = wf();
        let c = command(&w);
        assert_eq!(c.name, "fix");
        assert_eq!(c.workflow, "fix-bug");
        assert_eq!(
            c.usage(),
            "/fix <issue> [--dry] [--max <number>] [--scope <list>]"
        );
        let h = help(&w);
        assert!(
            h.starts_with("Reproduce, fix and prove a bug fix\n\nUsage: /fix <issue>"),
            "{h}"
        );
        assert!(
            h.contains("issue  string, required, position 1. The bug"),
            "{h}"
        );
        assert!(h.contains("scope  list, default [\"src/**\"]"), "{h}");
        assert!(h.contains("Steps: a"));
        let named = parse("name: x\nsteps:\n  - id: a\n    run: x\n", None).unwrap();
        assert_eq!(command(&named).name, "x", "command defaults to the name");
        assert_eq!(command(&named).description, "Run workflow x");
        assert_eq!(
            commands([&w, &named])
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["fix", "x"]
        );
    }

    #[test]
    fn maps_arguments_onto_inputs() {
        let w = wf();
        let p = parse_args(
            &w,
            "header parsing breaks on CRLF --scope src/**,tests/** --max 3 --dry",
        )
        .unwrap();
        assert!(p.is_complete());
        assert_eq!(p.inputs["issue"], "header parsing breaks on CRLF");
        assert_eq!(p.inputs["scope"], json!(["src/**", "tests/**"]));
        assert_eq!(p.inputs["max"], 3);
        assert_eq!(p.inputs["dry"], true);
        let p = parse_args(&w, "\"quoted issue\" scope=a --scope=b --dry false").unwrap();
        assert_eq!(p.inputs["issue"], "quoted issue");
        assert_eq!(p.inputs["scope"], json!(["a", "b"]));
        assert_eq!(p.inputs["dry"], false);
        assert!(
            parse_args(&w, "--nope 1")
                .unwrap_err()
                .contains("unknown input `nope`")
        );
        assert!(
            parse_args(&w, "x --max many")
                .unwrap_err()
                .contains("must be a number")
        );
        assert!(
            parse_args(&w, "x --max")
                .unwrap_err()
                .contains("needs a value")
        );
    }

    #[test]
    fn missing_required_inputs_are_returned_for_prompting() {
        let w = wf();
        let p = parse_args(&w, "--dry").unwrap();
        assert_eq!(p.missing.len(), 1);
        assert_eq!(p.missing[0].name, "issue");
        assert_eq!(p.missing[0].prompt(), "issue (The bug)");
        assert_eq!(p.missing[0].hint(), "text");
        let done = complete(&w, &p, &[("issue".into(), "it crashes".into())]).unwrap();
        assert_eq!(done["issue"], "it crashes");
        assert_eq!(done["scope"], json!(["src/**"]), "defaults filled in");
        assert!(complete(&w, &p, &[("issue".into(), " ".into())]).is_err());
        assert!(
            complete(&w, &p, &[])
                .unwrap_err()
                .contains("missing required input `issue`")
        );
    }
}
