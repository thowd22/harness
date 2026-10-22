//! Workflow templating: minijinja over a fixed, explicit context.
//!
//! Templates (prompts, commands, `if`, `loop.until`, `for_each`, `scope`,
//! `goal`, ...) see exactly these variables and nothing else:
//!
//! | Variable | Meaning |
//! |---|---|
//! | `inputs.<name>` | workflow inputs |
//! | `steps.<id>.outputs.<name>` | a finished step's outputs |
//! | `steps.<id>.status` | `succeeded`, `failed`, `skipped`, `cancelled` |
//! | `steps.<id>.artifacts.<name>` | path of a step's artifact |
//! | `matrix.<key>` | the current matrix combination |
//! | `item` | the current `for_each` element |
//! | `run.id`, `run.dir` | the run id and run directory |
//!
//! There is no access to environment variables, files or secrets: the
//! environment has no loader, and the context is built only from the
//! values above. Undefined variables are errors (strict mode).
//!
//! References are checked statically at load time ([`references`],
//! [`check_refs`]), so a typo in `steps.repro.outputs.tset_name` is a load
//! error, not a runtime surprise.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use minijinja::{Environment, UndefinedBehavior};
use serde::Serialize;
use serde_json::Value;

/// Top-level variables a template may use.
pub const ROOTS: &[&str] = &["inputs", "steps", "matrix", "item", "run"];
/// Fields of `steps.<id>`.
pub const STEP_FIELDS: &[&str] = &["outputs", "status", "artifacts"];
/// Fields of `run`.
pub const RUN_FIELDS: &[&str] = &["id", "dir"];
/// minijinja globals that are harmless and allowed.
pub const ALLOWED_GLOBALS: &[&str] = &[
    "range",
    "dict",
    "namespace",
    "loop",
    "true",
    "false",
    "none",
];

/// A template failed to parse or render.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TemplateError {
    /// What went wrong.
    pub message: String,
}

impl TemplateError {
    fn from_mj(e: &minijinja::Error) -> Self {
        let mut message = e.to_string();
        if let Some(detail) = e.detail()
            && !message.contains(detail)
        {
            message.push_str(": ");
            message.push_str(detail);
        }
        Self { message }
    }
}

fn env() -> &'static Environment<'static> {
    static ENV: OnceLock<Environment<'static>> = OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        // `debug` would dump the whole context into a prompt; not needed.
        env.remove_global("debug");
        env
    })
}

/// True when `s` contains template syntax.
pub fn is_template(s: &str) -> bool {
    s.contains("{{") || s.contains("{%")
}

/// The expression inside a string that is exactly `{{ expr }}`.
pub fn sole_expression(s: &str) -> Option<&str> {
    let t = s.trim();
    let inner = t.strip_prefix("{{")?.strip_suffix("}}")?;
    (!inner.contains("{{") && !inner.contains("}}")).then_some(inner.trim())
}

/// Renders `template` against `ctx`.
pub fn render(template: &str, ctx: &Value) -> Result<String, TemplateError> {
    if !is_template(template) {
        return Ok(template.to_owned());
    }
    env()
        .render_str(
            template,
            minijinja::Value::from(minijinja::value::Serde(ctx)),
        )
        .map_err(|e| TemplateError::from_mj(&e))
}

fn expr_text(s: &str) -> &str {
    sole_expression(s).unwrap_or(s.trim())
}

/// Evaluates an expression (`a == 'b'` or `{{ a == 'b' }}`) to a value.
pub fn eval(expr: &str, ctx: &Value) -> Result<Value, TemplateError> {
    let e = env()
        .compile_expression(expr_text(expr))
        .map_err(|e| TemplateError::from_mj(&e))?;
    let v = e
        .eval(minijinja::Value::from(minijinja::value::Serde(ctx)))
        .map_err(|e| TemplateError::from_mj(&e))?;
    serde_json::to_value(&v).map_err(|e| TemplateError {
        message: e.to_string(),
    })
}

/// Evaluates a condition with Jinja truthiness.
pub fn eval_bool(expr: &str, ctx: &Value) -> Result<bool, TemplateError> {
    let e = env()
        .compile_expression(expr_text(expr))
        .map_err(|e| TemplateError::from_mj(&e))?;
    e.eval(minijinja::Value::from(minijinja::value::Serde(ctx)))
        .map(|v| v.is_true())
        .map_err(|e| TemplateError::from_mj(&e))
}

/// Renders a value: a string that is exactly `{{ expr }}` keeps the
/// expression's type (a list stays a list); other strings render to text;
/// other JSON values are returned unchanged (strings inside are rendered).
pub fn render_value(v: &Value, ctx: &Value) -> Result<Value, TemplateError> {
    match v {
        Value::String(s) if sole_expression(s).is_some() => eval(s, ctx),
        Value::String(s) => render(s, ctx).map(Value::String),
        Value::Array(a) => a
            .iter()
            .map(|x| render_value(x, ctx))
            .collect::<Result<_, _>>()
            .map(Value::Array),
        Value::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, x) in m {
                out.insert(k.clone(), render_value(x, ctx)?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

/// The variables a template refers to, as dotted paths
/// (`steps.repro.outputs.test_name`). Errors when it does not parse.
pub fn references(template: &str) -> Result<BTreeSet<String>, TemplateError> {
    if !is_template(template) {
        return Ok(BTreeSet::new());
    }
    let t = env()
        .template_from_str(template)
        .map_err(|e| TemplateError::from_mj(&e))?;
    Ok(t.undeclared_variables(true).into_iter().collect())
}

/// Like [`references`] for a bare or braced expression.
pub fn expr_references(expr: &str) -> Result<BTreeSet<String>, TemplateError> {
    let e = env()
        .compile_expression(expr_text(expr))
        .map_err(|e| TemplateError::from_mj(&e))?;
    Ok(e.undeclared_variables(true).into_iter().collect())
}

/// What a step offers to templates, for static checks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepRefs {
    /// Declared output names; `None` when any name may appear (nested
    /// workflows).
    pub outputs: Option<BTreeSet<String>>,
    /// Declared artifact names.
    pub artifacts: BTreeSet<String>,
}

/// What a template at one location may refer to.
#[derive(Debug, Clone, Default)]
pub struct RefScope<'a> {
    /// Declared inputs.
    pub inputs: Option<&'a BTreeSet<String>>,
    /// Every step of the workflow.
    pub steps: Option<&'a BTreeMap<String, StepRefs>>,
    /// Steps whose results exist when the template renders (upstream).
    pub visible: BTreeSet<String>,
    /// Matrix keys of the current step, if it has a matrix.
    pub matrix: Option<BTreeSet<String>>,
    /// True when the step has `for_each`.
    pub item: bool,
}

/// Checks dotted references against `scope`; returns one message per
/// problem.
pub fn check_refs(refs: &BTreeSet<String>, scope: &RefScope<'_>) -> Vec<String> {
    let mut errs = Vec::new();
    for r in refs {
        let parts: Vec<&str> = r.split('.').collect();
        let root = parts[0];
        match root {
            "inputs" => {
                if let (Some(name), Some(inputs)) = (parts.get(1), scope.inputs)
                    && !inputs.contains(*name)
                {
                    errs.push(format!(
                        "`{r}`: unknown input `{name}` (declared: {})",
                        list(inputs.iter())
                    ));
                }
            }
            "steps" => {
                let Some(id) = parts.get(1) else { continue };
                let Some(steps) = scope.steps else { continue };
                let Some(info) = steps.get(*id) else {
                    errs.push(format!("`{r}`: unknown step `{id}`"));
                    continue;
                };
                if !scope.visible.contains(*id) {
                    errs.push(format!(
                        "`{r}`: step `{id}` is not upstream of this step; add it to `needs`"
                    ));
                    continue;
                }
                match parts.get(2) {
                    None => {}
                    Some(&"outputs") => {
                        if let (Some(name), Some(outs)) = (parts.get(3), &info.outputs)
                            && !outs.contains(*name)
                        {
                            errs.push(format!(
                                "`{r}`: step `{id}` has no output `{name}` (outputs: {})",
                                list(outs.iter())
                            ));
                        }
                    }
                    Some(&"artifacts") => {
                        if let Some(name) = parts.get(3)
                            && !info.artifacts.contains(*name)
                        {
                            errs.push(format!(
                                "`{r}`: step `{id}` has no artifact `{name}` (artifacts: {})",
                                list(info.artifacts.iter())
                            ));
                        }
                    }
                    Some(&"status") => {}
                    Some(other) => errs.push(format!(
                        "`{r}`: unknown field `{other}` (use {})",
                        STEP_FIELDS.join(", ")
                    )),
                }
            }
            "matrix" => match &scope.matrix {
                None => errs.push(format!("`{r}`: this step has no `matrix`")),
                Some(keys) => {
                    if let Some(k) = parts.get(1)
                        && !keys.contains(*k)
                    {
                        errs.push(format!(
                            "`{r}`: unknown matrix key `{k}` (keys: {})",
                            list(keys.iter())
                        ));
                    }
                }
            },
            "item" => {
                if !scope.item {
                    errs.push(format!("`{r}`: `item` is only defined in `for_each` steps"));
                }
            }
            "run" => {
                if let Some(f) = parts.get(1)
                    && !RUN_FIELDS.contains(f)
                {
                    errs.push(format!("`{r}`: unknown field `{f}` (use run.id or run.dir)"));
                }
            }
            g if ALLOWED_GLOBALS.contains(&g) => {}
            other => errs.push(format!(
                "`{r}`: unknown variable `{other}` (templates can use {}; environment variables and secrets are not available)",
                ROOTS.join(", ")
            )),
        }
    }
    errs
}

fn list<'a>(it: impl Iterator<Item = &'a String>) -> String {
    let v: Vec<&str> = it.map(String::as_str).collect();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

/// `steps.<id>` as seen by templates.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StepView {
    /// Outputs.
    pub outputs: serde_json::Map<String, Value>,
    /// Status name.
    pub status: String,
    /// Artifact name to path.
    pub artifacts: BTreeMap<String, String>,
}

/// `run` as seen by templates.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RunView {
    /// Run id.
    pub id: String,
    /// Run directory.
    pub dir: String,
}

/// The full template context.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Context {
    /// Workflow inputs.
    pub inputs: serde_json::Map<String, Value>,
    /// Finished steps.
    pub steps: BTreeMap<String, StepView>,
    /// Current matrix combination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix: Option<serde_json::Map<String, Value>>,
    /// Current `for_each` element.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<Value>,
    /// The run.
    pub run: RunView,
}

impl Context {
    /// As a JSON value for [`render`] and friends.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests;
