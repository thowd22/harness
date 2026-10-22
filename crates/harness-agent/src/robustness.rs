//! Tool-call robustness: lenient JSON repair, argument validation against the
//! tool's JSON schema, short actionable error messages and detection of
//! repeated identical failing calls.
//!
//! Everything here is pure and synchronous so the agent loop and the tool
//! dispatcher can share it and it can be tested in isolation.
//!
//! - [`parse_arguments`] turns the raw `input` of a `tool_use` block into a
//!   JSON object, repairing common defects ([`repair_json`]) and recording
//!   each repair so it can be logged.
//! - [`validate`] checks (and safely coerces) arguments against the subset of
//!   JSON Schema the harness tools use.
//! - [`ToolErrorMessage`] renders the text sent back to the model: what went
//!   wrong, and how to correct the call.
//! - [`LoopDetector`] notices when the model repeats the same failing call
//!   and produces a nudge.

use std::collections::HashMap;
use std::fmt;

use serde_json::{Map, Value};

/// Maximum length of an error message sent to the model, in characters.
pub const MAX_ERROR_CHARS: usize = 1200;

/// Number of identical failing calls that triggers loop detection.
pub const LOOP_THRESHOLD: u32 = 3;

/// One fix applied to a tool call's arguments before it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repair(pub String);

impl fmt::Display for Repair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Arguments after parsing and repair.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedArgs {
    /// The argument object.
    pub value: Value,
    /// Repairs applied, in order (empty when the input was valid).
    pub repairs: Vec<Repair>,
}

/// Parses the `input` of a `tool_use` block into a JSON object.
///
/// Providers keep arguments that are not valid JSON as a JSON string; those
/// are repaired with [`repair_json`]. A string that holds a JSON object
/// (double encoded arguments) is unwrapped. `null` becomes `{}`.
///
/// Returns a model-facing reason when the arguments cannot be recovered.
pub fn parse_arguments(input: &Value) -> Result<ParsedArgs, String> {
    match input {
        Value::Object(_) => Ok(ParsedArgs {
            value: input.clone(),
            repairs: Vec::new(),
        }),
        Value::Null => Ok(ParsedArgs {
            value: Value::Object(Map::new()),
            repairs: vec![Repair("treated null arguments as {}".into())],
        }),
        Value::String(raw) => {
            let mut parsed = repair_json(raw)?;
            if raw.trim_start().starts_with('{') && parsed.repairs.is_empty() {
                parsed
                    .repairs
                    .push(Repair("decoded arguments sent as a JSON string".into()));
            }
            match parsed.value {
                Value::Object(_) => Ok(parsed),
                other => Err(format!(
                    "arguments must be a JSON object, got {}",
                    type_name(&other)
                )),
            }
        }
        other => Err(format!(
            "arguments must be a JSON object, got {}",
            type_name(other)
        )),
    }
}

/// Parses `raw` as JSON, repairing common defects when strict parsing fails.
///
/// Fixed defects: Markdown code fences and prose around the JSON, trailing
/// commas, single-quoted strings, raw newlines and tabs inside strings,
/// Python literals (`True`, `False`, `None`), truncated strings, a dangling
/// `,` or `:` at the end, unclosed objects and arrays, and trailing text
/// after the value. Every fix is reported in [`ParsedArgs::repairs`].
pub fn repair_json(raw: &str) -> Result<ParsedArgs, String> {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return Ok(ParsedArgs {
            value,
            repairs: Vec::new(),
        });
    }
    let mut repairs = Vec::new();
    let mut text = raw.trim();
    if let Some(inner) = strip_fence(text) {
        repairs.push(Repair("removed Markdown code fence".into()));
        text = inner;
    }
    if let Some(start) = text.find(['{', '[']) {
        if start > 0 {
            repairs.push(Repair("dropped text before the JSON value".into()));
            text = &text[start..];
        }
    } else {
        return Err(
            "arguments are not JSON: expected an object such as {\"key\": \"value\"}".into(),
        );
    }
    let fixed = Fixer::run(text, &mut repairs);
    match serde_json::from_str::<Value>(&fixed) {
        Ok(value) => {
            if !repairs.is_empty() {
                tracing::info!(target: "harness::tools", repairs = ?repairs, "repaired tool-call JSON");
            }
            Ok(ParsedArgs { value, repairs })
        }
        Err(e) => Err(format!("arguments are not valid JSON ({e})")),
    }
}

fn strip_fence(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("```")?;
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric());
    let rest = rest.strip_suffix("```").unwrap_or(rest);
    Some(rest.trim())
}

/// Single pass character fixer used by [`repair_json`].
struct Fixer<'r> {
    out: String,
    stack: Vec<char>,
    repairs: &'r mut Vec<Repair>,
    noted: Vec<&'static str>,
}

impl<'r> Fixer<'r> {
    fn note(&mut self, what: &'static str) {
        if !self.noted.contains(&what) {
            self.noted.push(what);
            self.repairs.push(Repair(what.into()));
        }
    }

    fn last_significant(&self) -> Option<char> {
        self.out.chars().rev().find(|c| !c.is_whitespace())
    }

    fn drop_trailing_comma(&mut self) -> bool {
        let trimmed = self.out.trim_end().len();
        if self.out[..trimmed].ends_with(',') {
            self.out.truncate(trimmed - 1);
            true
        } else {
            false
        }
    }

    fn run(text: &str, repairs: &'r mut Vec<Repair>) -> String {
        let mut f = Fixer {
            out: String::with_capacity(text.len() + 8),
            stack: Vec::new(),
            repairs,
            noted: Vec::new(),
        };
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        let mut in_string: Option<char> = None;
        let mut started = false;
        while i < chars.len() {
            let c = chars[i];
            if let Some(quote) = in_string {
                match c {
                    '\\' => {
                        f.out.push('\\');
                        if let Some(&n) = chars.get(i + 1) {
                            f.out.push(n);
                            i += 1;
                        }
                    }
                    '\n' => {
                        f.note("escaped raw newline inside a string");
                        f.out.push_str("\\n");
                    }
                    '\t' => {
                        f.note("escaped raw tab inside a string");
                        f.out.push_str("\\t");
                    }
                    '"' if quote == '\'' => f.out.push_str("\\\""),
                    c if c == quote => {
                        f.out.push('"');
                        in_string = None;
                    }
                    c => f.out.push(c),
                }
                i += 1;
                continue;
            }
            if started && f.stack.is_empty() {
                if !chars[i..].iter().all(|c| c.is_whitespace()) {
                    f.note("dropped text after the JSON value");
                }
                break;
            }
            match c {
                '"' => {
                    in_string = Some('"');
                    f.out.push('"');
                }
                '\'' => {
                    f.note("converted single-quoted strings to double quotes");
                    in_string = Some('\'');
                    f.out.push('"');
                }
                '{' | '[' => {
                    started = true;
                    f.stack.push(if c == '{' { '}' } else { ']' });
                    f.out.push(c);
                }
                '}' | ']' => {
                    if f.drop_trailing_comma() {
                        f.note("removed trailing comma");
                    }
                    if f.stack.last() == Some(&c) {
                        f.stack.pop();
                    } else {
                        // Mismatched closer: close what is open up to a match.
                        while let Some(top) = f.stack.pop() {
                            f.out.push(top);
                            if top == c {
                                break;
                            }
                        }
                        f.note("fixed mismatched brackets");
                        i += 1;
                        continue;
                    }
                    f.out.push(c);
                }
                c if c.is_ascii_alphabetic() => {
                    let start = i;
                    while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                        i += 1;
                    }
                    let word: String = chars[start..i].iter().collect();
                    let mapped = match word.as_str() {
                        "True" => Some("true"),
                        "False" => Some("false"),
                        "None" => Some("null"),
                        _ => None,
                    };
                    match mapped {
                        Some(m) => {
                            f.note("converted Python literals to JSON");
                            f.out.push_str(m);
                        }
                        None => f.out.push_str(&word),
                    }
                    continue;
                }
                c => f.out.push(c),
            }
            i += 1;
        }
        if in_string.is_some() {
            f.note("closed a truncated string");
            f.out.push('"');
        }
        if !f.stack.is_empty() {
            match f.last_significant() {
                Some(',') => {
                    f.drop_trailing_comma();
                }
                Some(':') => f.out.push_str("null"),
                _ => {}
            }
            f.note("closed unterminated objects or arrays (truncated JSON)");
            while let Some(top) = f.stack.pop() {
                f.out.push(top);
            }
        }
        f.out
    }
}

/// One problem found by [`validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgIssue {
    /// JSON path of the offending value (`command`, `items[2]`, or empty for
    /// the root).
    pub path: String,
    /// What is wrong, for the model.
    pub message: String,
}

impl fmt::Display for ArgIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "`{}` {}", self.path, self.message)
        }
    }
}

/// Validates `args` against `schema`, coercing safe mismatches in place.
///
/// Supported keywords: `type` (string or list), `properties`, `required`,
/// `additionalProperties: false`, `enum`, `minimum`, `maximum`, `items`.
/// Coercions (each pushed to `repairs`): numeric strings for `integer` and
/// `number`, `"true"`/`"false"` for `boolean`, and dropping `null` for an
/// optional property.
pub fn validate(
    schema: &Value,
    args: &mut Value,
    repairs: &mut Vec<Repair>,
) -> Result<(), Vec<ArgIssue>> {
    let mut issues = Vec::new();
    check(schema, args, "", repairs, &mut issues);
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn types_of(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn matches_type(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        _ => true,
    }
}

fn coerce(t: &str, v: &Value) -> Option<Value> {
    let s = v.as_str()?.trim();
    match t {
        "integer" => s.parse::<i64>().ok().map(Value::from),
        "number" => s
            .parse::<f64>()
            .ok()
            .and_then(|f| serde_json::Number::from_f64(f).map(Value::Number)),
        "boolean" => match s {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => None,
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_f64() => "number",
        Value::Number(_) => "integer",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 40 {
        format!("{}...", s.chars().take(40).collect::<String>())
    } else {
        s
    }
}

fn check(
    schema: &Value,
    v: &mut Value,
    path: &str,
    repairs: &mut Vec<Repair>,
    issues: &mut Vec<ArgIssue>,
) {
    let types = types_of(schema);
    if !types.is_empty() && !types.iter().any(|t| matches_type(t, v)) {
        match types.iter().find_map(|t| coerce(t, v)) {
            Some(c) => {
                repairs.push(Repair(format!(
                    "coerced `{path}` from {} to {}",
                    short(v),
                    type_name(&c)
                )));
                *v = c;
            }
            None => {
                issues.push(ArgIssue {
                    path: path.to_owned(),
                    message: format!(
                        "must be {}, got {} {}",
                        types.join(" or "),
                        type_name(v),
                        short(v)
                    ),
                });
                return;
            }
        }
    }
    if let Some(Value::Array(allowed)) = schema.get("enum")
        && !allowed.contains(v)
    {
        let list: Vec<String> = allowed.iter().map(Value::to_string).collect();
        issues.push(ArgIssue {
            path: path.to_owned(),
            message: format!("must be one of {}, got {}", list.join(", "), short(v)),
        });
    }
    if let Some(n) = v.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && n < min
        {
            issues.push(ArgIssue {
                path: path.to_owned(),
                message: format!("must be at least {min}, got {n}"),
            });
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && n > max
        {
            issues.push(ArgIssue {
                path: path.to_owned(),
                message: format!("must be at most {max}, got {n}"),
            });
        }
    }
    match v {
        Value::Object(map) => check_object(schema, map, path, repairs, issues),
        Value::Array(items) => {
            if let Some(item_schema) = schema.get("items") {
                for (i, item) in items.iter_mut().enumerate() {
                    check(item_schema, item, &format!("{path}[{i}]"), repairs, issues);
                }
            }
        }
        _ => {}
    }
}

fn check_object(
    schema: &Value,
    map: &mut Map<String, Value>,
    path: &str,
    repairs: &mut Vec<Repair>,
    issues: &mut Vec<ArgIssue>,
) {
    let empty = Map::new();
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    // Optional properties sent as null are dropped (models often do this).
    let nulls: Vec<String> = map
        .iter()
        .filter(|(k, v)| v.is_null() && !required.contains(&k.as_str()) && props.contains_key(*k))
        .filter(|(k, _)| !types_of(&props[*k]).contains(&"null"))
        .map(|(k, _)| k.clone())
        .collect();
    for k in nulls {
        map.remove(&k);
        repairs.push(Repair(format!(
            "dropped null optional field `{}`",
            join(path, &k)
        )));
    }
    for req in &required {
        if !map.contains_key(*req) {
            issues.push(ArgIssue {
                path: join(path, req),
                message: "is required but missing".into(),
            });
        }
    }
    let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        match props.get(&key) {
            Some(prop) => {
                if let Some(value) = map.get_mut(&key) {
                    check(prop, value, &join(path, &key), repairs, issues);
                }
            }
            None if closed => {
                let known: Vec<&str> = props.keys().map(String::as_str).collect();
                let hint = closest(&key, &known)
                    .map(|k| format!(" (did you mean `{k}`?)"))
                    .unwrap_or_default();
                issues.push(ArgIssue {
                    path: join(path, &key),
                    message: format!(
                        "is not a known field{hint}; allowed fields: {}",
                        known.join(", ")
                    ),
                });
            }
            None => {}
        }
    }
}

/// The candidate closest to `word` by edit distance, if reasonably close.
pub fn closest<'a>(word: &str, candidates: &[&'a str]) -> Option<&'a str> {
    candidates
        .iter()
        .map(|c| {
            let (w, l) = (word.to_lowercase(), c.to_lowercase());
            let d = if is_subsequence(&w, &l) || is_subsequence(&l, &w) {
                1
            } else {
                levenshtein(&w, &l)
            };
            (d, *c)
        })
        .filter(|(d, c)| *d <= (c.len().max(word.len()) / 2).max(1))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// True when every char of `short` appears in `long` in order (`cmd` in `command`).
fn is_subsequence(short: &str, long: &str) -> bool {
    let mut it = long.chars();
    short.len() >= 2 && short.chars().all(|c| it.any(|l| l == c))
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// A compact description of the expected arguments, e.g.
/// `{"command": string, "timeout"?: integer}`.
pub fn schema_summary(schema: &Value) -> String {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return "{}".into();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let fields: Vec<String> = props
        .iter()
        .map(|(k, p)| {
            let opt = if required.contains(&k.as_str()) {
                ""
            } else {
                "?"
            };
            let ty = types_of(p);
            let ty = if ty.is_empty() {
                "any".to_owned()
            } else {
                ty.join("|")
            };
            format!("\"{k}\"{opt}: {ty}")
        })
        .collect();
    format!("{{{}}}", fields.join(", "))
}

/// A model-facing tool error: what failed and how to correct the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolErrorMessage {
    /// Tool name as the model wrote it.
    pub tool: String,
    /// What went wrong.
    pub problem: String,
    /// How to fix the call.
    pub fix: String,
}

impl ToolErrorMessage {
    /// A new message.
    pub fn new(
        tool: impl Into<String>,
        problem: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            tool: tool.into(),
            problem: problem.into(),
            fix: fix.into(),
        }
    }

    /// The model asked for a tool that is not offered.
    pub fn unknown_tool(tool: &str, available: &[&str]) -> Self {
        let hint = closest(tool, available)
            .map(|c| format!("Did you mean `{c}`? "))
            .unwrap_or_default();
        Self::new(
            tool,
            format!("there is no tool named `{tool}`"),
            format!("{hint}Use one of: {}.", available.join(", ")),
        )
    }

    /// Arguments could not be parsed as JSON.
    pub fn bad_json(tool: &str, reason: &str, schema: &Value) -> Self {
        Self::new(
            tool,
            reason.to_owned(),
            format!(
                "Send the arguments as one JSON object: {}.",
                schema_summary(schema)
            ),
        )
    }

    /// Arguments failed schema validation.
    pub fn invalid_args(tool: &str, issues: &[ArgIssue], schema: &Value) -> Self {
        let list: Vec<String> = issues.iter().take(5).map(ToString::to_string).collect();
        Self::new(
            tool,
            format!("invalid arguments: {}", list.join("; ")),
            format!(
                "Call `{tool}` again with arguments {}.",
                schema_summary(schema)
            ),
        )
    }

    /// The same call id appeared twice in one turn.
    pub fn duplicate_id(tool: &str, id: &str) -> Self {
        Self::new(
            tool,
            format!("call id `{id}` was used more than once in this turn"),
            "Each tool call in a turn needs its own id; this duplicate was renamed and run normally.",
        )
    }
}

impl fmt::Display for ToolErrorMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = format!(
            "Error in `{}`: {}. Fix: {}",
            self.tool, self.problem, self.fix
        );
        f.write_str(&truncate_chars(&text, MAX_ERROR_CHARS))
    }
}

/// Truncates `s` to at most `max` characters, marking the cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

/// Renders a JSON value with object keys sorted, so equal arguments always
/// produce the same text regardless of key order.
pub fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

/// A detected loop of identical failing calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopDetected {
    /// Tool name.
    pub tool: String,
    /// How many times this exact call has failed.
    pub failures: u32,
    /// Text appended to the tool result to nudge the model.
    pub nudge: String,
}

/// Counts identical failing calls (same tool, same canonical arguments).
///
/// A success of the same call clears its count. Every [`LOOP_THRESHOLD`]-th
/// identical failure yields a [`LoopDetected`].
#[derive(Debug, Default, Clone)]
pub struct LoopDetector {
    failures: HashMap<(String, String), u32>,
    threshold: u32,
}

impl LoopDetector {
    /// A detector with the default threshold.
    pub fn new() -> Self {
        Self::with_threshold(LOOP_THRESHOLD)
    }

    /// A detector that fires after `threshold` identical failures.
    pub fn with_threshold(threshold: u32) -> Self {
        Self {
            failures: HashMap::new(),
            threshold: threshold.max(1),
        }
    }

    /// Records the outcome of one call. Returns a nudge when this exact
    /// failing call has now been repeated `threshold` times.
    pub fn observe(&mut self, tool: &str, args: &Value, failed: bool) -> Option<LoopDetected> {
        let key = (tool.to_owned(), canonical_json(args));
        if !failed {
            self.failures.remove(&key);
            return None;
        }
        let n = self.failures.entry(key).or_insert(0);
        *n += 1;
        (*n).is_multiple_of(self.threshold).then(|| LoopDetected {
            tool: tool.to_owned(),
            failures: *n,
            nudge: format!(
                "Loop detected: this exact `{tool}` call has failed {n} times with the same arguments. \
                 Repeating it will not help. Read the error, change the arguments, or try a different approach."
            ),
        })
    }
}

/// Result text for a `tool_use` that never got a result (crash, cancel or a
/// provider that dropped it).
pub const MISSING_RESULT: &str = "Error: this tool call has no recorded result (it was interrupted). Fix: check the current state before repeating it.";

/// Makes the conversation valid for providers: every `tool_use` in an
/// assistant message gets a `tool_result` in the user message right after
/// it. Missing results are synthesized as errors ([`MISSING_RESULT`]) and a
/// user message is inserted when there is none. Returns how many results
/// were added.
pub fn ensure_tool_results(messages: &mut Vec<harness_providers::model::Message>) -> usize {
    use harness_providers::model::{ContentBlock, Message, Role};
    let mut added = 0;
    let mut i = 0;
    while i < messages.len() {
        if messages[i].role != Role::Assistant {
            i += 1;
            continue;
        }
        let ids: Vec<String> = messages[i]
            .tool_uses()
            .map(|(id, _, _)| id.to_owned())
            .collect();
        if ids.is_empty() {
            i += 1;
            continue;
        }
        let next_is_user = messages.get(i + 1).is_some_and(|m| m.role == Role::User);
        if !next_is_user {
            messages.insert(
                i + 1,
                Message {
                    role: Role::User,
                    content: Vec::new(),
                },
            );
        }
        let reply = &mut messages[i + 1];
        let answered: Vec<String> = reply
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .collect();
        let mut missing: Vec<ContentBlock> = ids
            .iter()
            .filter(|id| !answered.contains(id))
            .map(|id| ContentBlock::tool_result(id.clone(), MISSING_RESULT, true))
            .collect();
        added += missing.len();
        if !missing.is_empty() {
            // Tool results go first in the user message.
            let pos = reply
                .content
                .iter()
                .rposition(|b| matches!(b, ContentBlock::ToolResult { .. }))
                .map_or(0, |p| p + 1);
            let tail = reply.content.split_off(pos);
            reply.content.append(&mut missing);
            reply.content.extend(tail);
        }
        i += 2;
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn repaired(raw: &str) -> (Value, Vec<String>) {
        let p = repair_json(raw).unwrap();
        (p.value, p.repairs.into_iter().map(|r| r.0).collect())
    }

    #[test]
    fn valid_json_has_no_repairs() {
        let (v, r) = repaired(r#"{"a": 1}"#);
        assert_eq!(v, json!({"a": 1}));
        assert!(r.is_empty());
    }

    #[test]
    fn trailing_commas_are_removed() {
        let (v, r) = repaired(r#"{"a": [1, 2,], "b": 3,}"#);
        assert_eq!(v, json!({"a": [1, 2], "b": 3}));
        assert_eq!(r, vec!["removed trailing comma"]);
    }

    #[test]
    fn truncated_string_and_object_are_closed() {
        let (v, r) = repaired(r#"{"command": "cargo te"#);
        assert_eq!(v, json!({"command": "cargo te"}));
        assert!(r.contains(&"closed a truncated string".to_owned()));
        assert!(r.iter().any(|r| r.contains("unterminated")));
    }

    #[test]
    fn dangling_comma_and_colon_are_fixed() {
        assert_eq!(repaired(r#"{"a": 1,"#).0, json!({"a": 1}));
        assert_eq!(repaired(r#"{"a": 1, "b":"#).0, json!({"a": 1, "b": null}));
        assert_eq!(
            repaired(r#"{"a": [1, {"b": 2"#).0,
            json!({"a": [1, {"b": 2}]})
        );
    }

    #[test]
    fn fences_prose_quotes_and_python_literals() {
        let (v, r) = repaired("```json\n{'path': 'a.rs', 'all': True, 'x': None}\n```");
        assert_eq!(v, json!({"path": "a.rs", "all": true, "x": null}));
        assert!(r.contains(&"removed Markdown code fence".to_owned()));
        assert!(r.contains(&"converted single-quoted strings to double quotes".to_owned()));
        assert!(r.contains(&"converted Python literals to JSON".to_owned()));

        let (v, r) = repaired("Here you go: {\"a\": 1} hope that helps");
        assert_eq!(v, json!({"a": 1}));
        assert!(r.contains(&"dropped text before the JSON value".to_owned()));
        assert!(r.contains(&"dropped text after the JSON value".to_owned()));
    }

    #[test]
    fn raw_newlines_in_strings_are_escaped() {
        let (v, _) = repaired("{\"content\": \"line1\nline2\"}");
        assert_eq!(v, json!({"content": "line1\nline2"}));
    }

    #[test]
    fn python_words_inside_strings_are_kept() {
        let (v, _) = repaired(r#"{"a": "True story",}"#);
        assert_eq!(v, json!({"a": "True story"}));
    }

    #[test]
    fn hopeless_input_is_an_error() {
        assert!(repair_json("not json at all").is_err());
        assert!(repair_json("{\"a\" 1}").is_err());
    }

    #[test]
    fn parse_arguments_handles_strings_null_and_non_objects() {
        let p = parse_arguments(&json!("{\"a\": 1,}")).unwrap();
        assert_eq!(p.value, json!({"a": 1}));
        let p = parse_arguments(&json!("{\"a\": 1}")).unwrap();
        assert_eq!(p.repairs.len(), 1, "double encoding is logged");
        assert_eq!(parse_arguments(&Value::Null).unwrap().value, json!({}));
        assert!(
            parse_arguments(&json!([1]))
                .unwrap_err()
                .contains("JSON object")
        );
        assert!(
            parse_arguments(&json!("[1, 2]"))
                .unwrap_err()
                .contains("got array")
        );
    }

    fn bash_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout": {"type": "integer", "minimum": 1}
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    #[test]
    fn validation_reports_missing_unknown_and_wrong_types() {
        let mut args = json!({"cmd": "ls", "timeout": "soon"});
        let mut repairs = Vec::new();
        let issues = validate(&bash_schema(), &mut args, &mut repairs).unwrap_err();
        let text: Vec<String> = issues.iter().map(ToString::to_string).collect();
        assert!(
            text.iter()
                .any(|t| t == "`command` is required but missing"),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|t| t.contains("`cmd` is not a known field (did you mean `command`?)")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|t| t.contains("`timeout` must be integer, got string")),
            "{text:?}"
        );
    }

    #[test]
    fn validation_coerces_safe_mismatches() {
        let mut args = json!({"command": "ls", "timeout": "30"});
        let mut repairs = Vec::new();
        validate(&bash_schema(), &mut args, &mut repairs).unwrap();
        assert_eq!(args, json!({"command": "ls", "timeout": 30}));
        assert_eq!(repairs.len(), 1);

        let mut args = json!({"command": "ls", "timeout": null});
        validate(&bash_schema(), &mut args, &mut repairs).unwrap();
        assert_eq!(args, json!({"command": "ls"}));
    }

    #[test]
    fn validation_checks_minimum_enum_and_items() {
        let mut repairs = Vec::new();
        let mut args = json!({"command": "ls", "timeout": 0});
        let issues = validate(&bash_schema(), &mut args, &mut repairs).unwrap_err();
        assert!(issues[0].message.contains("at least 1"));

        let schema = json!({"type": "object", "properties": {
            "mode": {"enum": ["a", "b"]},
            "list": {"type": "array", "items": {"type": "integer"}}
        }});
        let mut args = json!({"mode": "c", "list": [1, "x"]});
        let issues = validate(&schema, &mut args, &mut repairs).unwrap_err();
        assert_eq!(issues.len(), 2);
        assert!(issues.iter().any(|i| i.path == "list[1]"));
        assert!(issues.iter().any(|i| i.path == "mode"));
    }

    #[test]
    fn error_messages_are_short_and_actionable() {
        let issues = vec![ArgIssue {
            path: "command".into(),
            message: "is required but missing".into(),
        }];
        let msg = ToolErrorMessage::invalid_args("bash", &issues, &bash_schema()).to_string();
        assert_eq!(
            msg,
            "Error in `bash`: invalid arguments: `command` is required but missing. \
             Fix: Call `bash` again with arguments {\"command\": string, \"timeout\"?: integer}."
        );
        let msg = ToolErrorMessage::unknown_tool("raed", &["read", "write"]).to_string();
        assert!(msg.contains("Did you mean `read`?"), "{msg}");
        let long = ToolErrorMessage::new("t", "x".repeat(5000), "y").to_string();
        assert_eq!(long.chars().count(), MAX_ERROR_CHARS);
    }

    #[test]
    fn loop_detector_fires_on_repeated_identical_failures() {
        let mut d = LoopDetector::new();
        let a = json!({"b": 1, "a": 2});
        let same = json!({"a": 2, "b": 1});
        assert!(d.observe("bash", &a, true).is_none());
        assert!(d.observe("bash", &same, true).is_none());
        let hit = d.observe("bash", &a, true).unwrap();
        assert_eq!(hit.failures, 3);
        assert!(hit.nudge.contains("Loop detected"));
        // A different call or a success resets nothing else / clears the key.
        assert!(d.observe("bash", &json!({"a": 3}), true).is_none());
        assert!(d.observe("bash", &a, false).is_none());
        assert!(d.observe("bash", &a, true).is_none());
    }

    #[test]
    fn missing_tool_results_are_synthesized() {
        use harness_providers::model::{ContentBlock, Message, Role};
        let call = |id: &str| ContentBlock::tool_use(id, "bash", json!({}));
        let mut msgs = vec![
            Message::user("hi"),
            Message {
                role: Role::Assistant,
                content: vec![call("a"), call("b")],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::tool_result("a", "ok", false),
                    ContentBlock::text("more"),
                ],
            },
            Message {
                role: Role::Assistant,
                content: vec![call("c")],
            },
        ];
        assert_eq!(ensure_tool_results(&mut msgs), 2);
        assert_eq!(msgs.len(), 5);
        let ids = |m: &Message| -> Vec<String> {
            m.content
                .iter()
                .map(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => tool_use_id.clone(),
                    ContentBlock::Text { text } => text.clone(),
                    _ => String::new(),
                })
                .collect()
        };
        assert_eq!(ids(&msgs[2]), ["a", "b", "more"]);
        assert_eq!(ids(&msgs[4]), ["c"]);
        assert_eq!(ensure_tool_results(&mut msgs), 0, "idempotent");
    }

    #[test]
    fn canonical_json_sorts_keys() {
        assert_eq!(
            canonical_json(&json!({"b": [1, {"d": 1, "c": 2}], "a": "x"})),
            r#"{"a":"x","b":[1,{"c":2,"d":1}]}"#
        );
    }
}
