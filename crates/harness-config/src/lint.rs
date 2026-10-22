//! Prompt lint: blanket prohibitions, cross-layer contradictions and token
//! budgets (`harness lint`, and warnings in `harness doctor`).
//!
//! Rules ([`Rule`]):
//!
//! - **blanket-prohibition** (error): sentences such as "DO NOT MODIFY
//!   tests" or "never change config". Models read these over-literally and
//!   refuse legitimate work. The finding suggests positive-scope wording and
//!   points at code-enforced alternatives (`scope:`, `policy:`).
//! - **contradiction** (error): one source forbids what another requires or
//!   permits ("never run git commit" vs "always run git commit"), a prompt
//!   forbids a tool the agent's `tools:` include, or a prompt tells the model
//!   to use a tool the agent does not have. Sources are prompt layers, inline
//!   `prompt_text`, skills and memory text supplied by the caller.
//! - **token-budget** (warning): a prompt (all layers) or the skill listing
//!   estimated above [`LintOptions::prompt_token_budget`] tokens
//!   (`ceil(bytes / 4)`).
//!
//! Findings that involve memory text are downgraded to warnings: memory is
//! data, not authored config. Load and validation errors from the
//! [`Catalog`] are reported as errors too, so `harness lint` exits non-zero
//! for any broken file.

use std::fmt;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;

use crate::agents::{Agent, MemoryAccess, PromptRef, ValidationContext};
use crate::discovery::{Catalog, Entry, Origin};
use crate::prompts::{LayerSource, Prompt};

/// Severity of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Reported; `harness lint` still exits 0. Shown by `harness doctor`.
    Warning,
    /// `harness lint` exits non-zero.
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Warning => "warning",
            Severity::Error => "error",
        })
    }
}

/// Lint rule identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    /// Prohibition over a broad category (tests, config, files...).
    BlanketProhibition,
    /// Conflicting instructions or instruction vs agent config.
    Contradiction,
    /// Prompt larger than the budget.
    TokenBudget,
    /// A file failed to load or validate.
    Invalid,
}

impl Rule {
    /// Kebab-case id shown in output.
    pub fn id(self) -> &'static str {
        match self {
            Rule::BlanketProhibition => "blanket-prohibition",
            Rule::Contradiction => "contradiction",
            Rule::TokenBudget => "token-budget",
            Rule::Invalid => "invalid",
        }
    }
}

/// A location: display name plus 1-based line/column (0 = unknown).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Loc {
    /// File path or `builtin:<rel>` or a label such as `memory:<id>`.
    pub file: String,
    /// 1-based line.
    pub line: usize,
    /// 1-based column.
    pub column: usize,
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file)?;
        if self.line > 0 {
            write!(f, ":{}", self.line)?;
            if self.column > 0 {
                write!(f, ":{}", self.column)?;
            }
        }
        Ok(())
    }
}

/// One lint finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Severity.
    pub severity: Severity,
    /// Rule.
    pub rule: Rule,
    /// Where.
    pub loc: Loc,
    /// What is wrong.
    pub message: String,
    /// How to fix it.
    pub suggestion: Option<String>,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} [{}]: {}",
            self.loc,
            self.severity,
            self.rule.id(),
            self.message
        )?;
        if let Some(s) = &self.suggestion {
            write!(f, "\n  help: {s}")?;
        }
        Ok(())
    }
}

/// What kind of text a [`LintSource`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A prompt layer.
    PromptLayer,
    /// An agent's inline `prompt_text`.
    AgentPrompt,
    /// A skill body.
    Skill,
    /// Memory content (findings are warnings).
    Memory,
}

/// A piece of instruction text to lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintSource {
    /// Kind.
    pub kind: SourceKind,
    /// Human label, e.g. ``prompt `coder` layer 2``.
    pub label: String,
    /// Display file for locations.
    pub file: String,
    /// The text to scan.
    pub text: String,
    /// The whole file the text lives in, for line numbers (e.g. the YAML
    /// around an inline layer). `None` means `text` is the whole file.
    pub file_text: Option<String>,
}

impl LintSource {
    /// A memory source (caller-supplied, e.g. from harness-memory).
    pub fn memory(
        label: impl Into<String>,
        file: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        LintSource {
            kind: SourceKind::Memory,
            label: label.into(),
            file: file.into(),
            text: text.into(),
            file_text: None,
        }
    }

    fn severity(&self, base: Severity) -> Severity {
        if self.kind == SourceKind::Memory {
            Severity::Warning
        } else {
            base
        }
    }

    fn loc_of(&self, byte: usize) -> Loc {
        let start = self.text[..byte].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[byte..]
            .find('\n')
            .map_or(self.text.len(), |i| byte + i);
        let line_text = &self.text[start..end];
        let col_in_line = byte - start;
        let (line, column) = match &self.file_text {
            None => (self.text[..byte].matches('\n').count() + 1, col_in_line + 1),
            Some(ft) => {
                let needle = line_text.trim();
                ft.lines()
                    .enumerate()
                    .find_map(|(i, l)| {
                        l.find(needle).map(|c| {
                            (
                                i + 1,
                                c + 1
                                    + col_in_line.saturating_sub(
                                        line_text.len() - line_text.trim_start().len(),
                                    ),
                            )
                        })
                    })
                    .unwrap_or((0, 0))
            }
        };
        Loc {
            file: self.file.clone(),
            line,
            column,
        }
    }
}

/// Lint options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LintOptions {
    /// Token budget for one prompt (all layers) or one inline `prompt_text`,
    /// and for the skill listing. Default 4000.
    pub prompt_token_budget: u64,
}

impl Default for LintOptions {
    fn default() -> Self {
        LintOptions {
            prompt_token_budget: 4000,
        }
    }
}

/// Estimated tokens: `ceil(bytes / 4)` (same estimate as tool output).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

/// Lint results.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LintReport {
    /// Findings, sorted by location.
    pub findings: Vec<Finding>,
}

impl LintReport {
    /// Error findings.
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
    }

    /// Warning findings (what `harness doctor` shows).
    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
    }

    /// True if any finding is an error.
    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }

    /// `"N errors, M warnings"`.
    pub fn summary(&self) -> String {
        let e = self.errors().count();
        let w = self.warnings().count();
        format!(
            "{e} error{}, {w} warning{}",
            if e == 1 { "" } else { "s" },
            if w == 1 { "" } else { "s" }
        )
    }

    fn sort(&mut self) {
        self.findings.sort_by(|a, b| {
            (&a.loc.file, a.loc.line, a.loc.column, a.rule, &a.message).cmp(&(
                &b.loc.file,
                b.loc.line,
                b.loc.column,
                b.rule,
                &b.message,
            ))
        });
        self.findings.dedup();
    }
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap_or_else(|e| panic!("bad lint regex: {e}")))
}

const NEG: &str = r"(?:do\s+not|don't|dont|never|must\s+not|mustn't|should\s+not|shouldn't|cannot|can't|may\s+not|are\s+not\s+allowed\s+to|not\s+allowed\s+to|under\s+no\s+circumstances)";

fn blanket_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        &format!(
            r"(?i)\b{NEG}\s+(?:\w+\s+)?(?:modify|change|edit|touch|delete|remove|alter|update|rewrite|write\s+to)\s+(?:(?:any|the|existing|these|those|all|other|your)\s+)*(tests?|test\s+files?|test\s+cases?|specs?|config(?:uration)?(?:\s+files?)?|configs|settings|dependencies|files|code|docs|documentation|ci)\b"
        ),
    )
}

fn bare_do_not_modify_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"(?m)\bDO\s+NOT\s+(?:MODIFY|EDIT|CHANGE|TOUCH)\b(?:\s*[.!:]|\s*$)",
    )
}

fn suggestion_for(category: &str) -> String {
    let c = category.to_lowercase();
    let example = if c.starts_with("test") || c.starts_with("spec") {
        "\"You may add or update tests for the code you change; keep existing assertions unless the task says they are wrong.\""
    } else if c.starts_with("config") || c == "settings" || c == "ci" || c == "dependencies" {
        "\"Change configuration only when the task needs it, and list every such change in your summary.\""
    } else {
        "\"You may edit files matching the task scope.\""
    };
    format!(
        "say what is allowed instead, e.g. {example} Hard limits belong in the agent's `scope:` or `policy:` (enforced in code), not in prose"
    )
}

/// Blanket prohibitions in one source.
pub fn blanket_prohibitions(src: &LintSource) -> Vec<Finding> {
    let mut out = Vec::new();
    for c in blanket_re().captures_iter(&src.text) {
        let (Some(m), Some(cat)) = (c.get(0), c.get(1)) else {
            continue;
        };
        out.push(Finding {
            severity: src.severity(Severity::Error),
            rule: Rule::BlanketProhibition,
            loc: src.loc_of(m.start()),
            message: format!(
                "{}: blanket prohibition \"{}\" over `{}`",
                src.label,
                m.as_str(),
                cat.as_str()
            ),
            suggestion: Some(suggestion_for(cat.as_str())),
        });
    }
    for m in bare_do_not_modify_re().find_iter(&src.text) {
        if out.iter().any(|f| f.loc == src.loc_of(m.start())) {
            continue;
        }
        out.push(Finding {
            severity: src.severity(Severity::Error),
            rule: Rule::BlanketProhibition,
            loc: src.loc_of(m.start()),
            message: format!(
                "{}: boilerplate \"{}\" without a scope",
                src.label,
                m.as_str().trim()
            ),
            suggestion: Some(suggestion_for("files")),
        });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Polarity {
    Forbid,
    Require,
}

#[derive(Debug, Clone)]
struct Directive {
    polarity: Polarity,
    key: String,
    phrase: String,
    byte: usize,
}

const STOP: &[&str] = &[
    "the", "a", "an", "any", "all", "your", "you", "to", "of", "existing", "these", "those",
];

fn directives(text: &str) -> Vec<Directive> {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = re(
        &R,
        &format!(
            r"(?i)\b(?P<neg>{NEG}|avoid)\s+(?P<a>[a-z][a-z0-9_\-]*(?:[ \t]+[a-z0-9_\-`.*]+){{0,3}})|\b(?P<pos>always|you\s+must|you\s+should|make\s+sure\s+to|be\s+sure\s+to|remember\s+to|you\s+may|you\s+can|feel\s+free\s+to)\s+(?P<b>[a-z][a-z0-9_\-]*(?:[ \t]+[a-z0-9_\-`.*]+){{0,3}})"
        ),
    );
    let mut out = Vec::new();
    for c in r.captures_iter(text) {
        let (polarity, m) = match (c.name("neg"), c.name("a"), c.name("b")) {
            (Some(_), Some(a), _) => (Polarity::Forbid, a),
            (None, _, Some(b)) => (Polarity::Require, b),
            _ => continue,
        };
        let mut words = Vec::new();
        let mut end = m.start();
        let mut off = m.start();
        for w in m.as_str().split_inclusive(char::is_whitespace) {
            let norm = w
                .trim()
                .trim_matches(|c: char| c == '`' || c == '.')
                .to_lowercase();
            let w_end = off + w.trim_end().len();
            off += w.len();
            if norm.is_empty() || STOP.contains(&norm.as_str()) {
                continue;
            }
            words.push(norm);
            end = w_end;
            if words.len() == 3 {
                break;
            }
        }
        if words.len() < 2 {
            continue;
        }
        let Some(whole) = c.get(0) else { continue };
        let phrase = text[whole.start()..end]
            .trim_end_matches(['.', ',', ';', ':', '!'])
            .to_owned();
        out.push(Directive {
            polarity,
            key: words.join(" "),
            phrase,
            byte: whole.start(),
        });
    }
    out
}

/// Directive contradictions across (and within) sources.
pub fn contradictions(sources: &[LintSource]) -> Vec<Finding> {
    contradictions_where(sources, |_, _| true)
}

/// Like [`contradictions`], keeping only pairs of source indices for which
/// `keep(forbidding, requiring)` is true.
fn contradictions_where(
    sources: &[LintSource],
    keep: impl Fn(usize, usize) -> bool,
) -> Vec<Finding> {
    let all: Vec<(usize, Directive)> = sources
        .iter()
        .enumerate()
        .flat_map(|(i, s)| directives(&s.text).into_iter().map(move |d| (i, d)))
        .collect();
    let mut out = Vec::new();
    for (ia, a) in all.iter().filter(|(_, d)| d.polarity == Polarity::Forbid) {
        for (ib, b) in all.iter().filter(|(_, d)| d.polarity == Polarity::Require) {
            if a.key != b.key || !keep(*ia, *ib) {
                continue;
            }
            let (sa, sb) = (&sources[*ia], &sources[*ib]);
            let sev = sa.severity(sb.severity(Severity::Error));
            let other = sb.loc_of(b.byte);
            out.push(Finding {
                severity: sev,
                rule: Rule::Contradiction,
                loc: sa.loc_of(a.byte),
                message: format!(
                    "{} says \"{}\" but {} ({other}) says \"{}\"",
                    sa.label, a.phrase, sb.label, b.phrase
                ),
                suggestion: Some("keep one instruction, scoped to when it applies".into()),
            });
        }
    }
    out
}

struct ToolWords {
    tool: &'static str,
    forbid: &'static str,
    mention: &'static str,
}

const TOOL_WORDS: &[ToolWords] = &[
    ToolWords {
        tool: "bash",
        forbid: r"(?:use|run|call|invoke|execute)\s+(?:the\s+)?(?:bash|shell|terminal|any\s+commands|shell\s+commands|commands)\b",
        mention: r"\b(?:use|call|with)\s+(?:the\s+)?`?bash`?(?:\s+tool)?\b",
    },
    ToolWords {
        tool: "write",
        forbid: r"(?:write|create)\s+(?:any\s+|new\s+)*files\b",
        mention: r"\b(?:use|call|with)\s+(?:the\s+)?`?write`?\s+tool\b",
    },
    ToolWords {
        tool: "edit",
        forbid: r"(?:edit|modify|change)\s+(?:any\s+)*files\b",
        mention: r"\b(?:use|call|with)\s+(?:the\s+)?`?edit`?\s+tool\b",
    },
    ToolWords {
        tool: "memory",
        forbid: r"(?:use|read|write|access)\s+(?:the\s+)?memory\b",
        mention: r"\b(?:use|call|with)\s+(?:the\s+)?`?memory`?\s+tool\b",
    },
    ToolWords {
        tool: "spawn_agent",
        forbid: r"(?:spawn|use|start|delegate\s+to)\s+(?:any\s+)?subagents?\b",
        mention: r"\b(?:use|call|with)\s+(?:the\s+)?`?spawn_agent`?(?:\s+tool)?\b",
    },
    ToolWords {
        tool: "todo",
        forbid: r"(?:use|update)\s+(?:the\s+)?todo(?:\s+list)?\b",
        mention: r"\b(?:use|call|with)\s+(?:the\s+)?`?todo`?(?:\s+tool)?\b",
    },
];

fn tool_regexes() -> &'static Vec<(&'static str, Regex, Regex)> {
    static R: OnceLock<Vec<(&'static str, Regex, Regex)>> = OnceLock::new();
    R.get_or_init(|| {
        TOOL_WORDS
            .iter()
            .filter_map(|t| {
                let f = Regex::new(&format!(r"(?i)\b{NEG}\s+{}", t.forbid)).ok()?;
                let m = Regex::new(&format!("(?i){}", t.mention)).ok()?;
                Some((t.tool, f, m))
            })
            .collect()
    })
}

/// Instruction-vs-config contradictions for one agent.
pub fn tool_contradictions(agent: &Agent, sources: &[LintSource]) -> Vec<Finding> {
    let tools = agent.request_tools();
    let mut out = Vec::new();
    for src in sources {
        for (tool, forbid, mention) in tool_regexes() {
            let has = tools.contains(tool);
            if has {
                for m in forbid.find_iter(&src.text) {
                    out.push(Finding {
                        severity: src.severity(Severity::Error),
                        rule: Rule::Contradiction,
                        loc: src.loc_of(m.start()),
                        message: format!(
                            "{} says \"{}\" but agent `{}` is given the `{tool}` tool",
                            src.label,
                            m.as_str(),
                            agent.name
                        ),
                        suggestion: Some(format!(
                            "remove `{tool}` from the agent's tools (or its enabling field), or drop the sentence"
                        )),
                    });
                }
            } else {
                for m in mention.find_iter(&src.text) {
                    out.push(Finding {
                        severity: src.severity(Severity::Error),
                        rule: Rule::Contradiction,
                        loc: src.loc_of(m.start()),
                        message: format!(
                            "{} says \"{}\" but agent `{}` does not have the `{tool}` tool",
                            src.label,
                            m.as_str(),
                            agent.name
                        ),
                        suggestion: Some(match *tool {
                            "spawn_agent" => {
                                "list allowed agents in `subagents:` or drop the sentence".into()
                            }
                            "memory" => {
                                "set `memory: read` or `read-write`, or drop the sentence".into()
                            }
                            _ => {
                                format!("add `{tool}` to the agent's `tools:` or drop the sentence")
                            }
                        }),
                    });
                }
            }
        }
    }
    out
}

fn yaml_dir_display(origin: &Origin, rel: &str) -> String {
    match &origin.path {
        Some(p) => p
            .parent()
            .unwrap_or(Path::new(""))
            .join(rel)
            .display()
            .to_string(),
        None => {
            let d = origin.display.rsplit_once('/').map_or("", |(d, _)| d);
            format!("{d}/{rel}")
        }
    }
}

/// Lint sources for each layer of a prompt.
pub fn prompt_sources(entry: &Entry<Prompt>) -> Vec<LintSource> {
    entry
        .item
        .layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let label = format!("prompt `{}` layer {}", entry.item.name, i + 1);
            match &l.source {
                LayerSource::Inline => LintSource {
                    kind: SourceKind::PromptLayer,
                    label,
                    file: entry.origin.display.clone(),
                    text: l.template.clone(),
                    file_text: Some(entry.text.clone()),
                },
                LayerSource::File(f) => LintSource {
                    kind: SourceKind::PromptLayer,
                    label,
                    file: yaml_dir_display(&entry.origin, f),
                    text: l.template.clone(),
                    file_text: None,
                },
            }
        })
        .collect()
}

fn agent_sources(entry: &Entry<Agent>, catalog: &Catalog) -> Vec<LintSource> {
    match &entry.item.prompt {
        PromptRef::Inline(t) => vec![LintSource {
            kind: SourceKind::AgentPrompt,
            label: format!("agent `{}` prompt_text", entry.item.name),
            file: entry.origin.display.clone(),
            text: t.clone(),
            file_text: Some(entry.text.clone()),
        }],
        PromptRef::Named(p) => catalog
            .prompts
            .get_entry(p)
            .map(prompt_sources)
            .unwrap_or_default(),
    }
}

fn skill_source(entry: &Entry<crate::skills::Skill>) -> LintSource {
    LintSource {
        kind: SourceKind::Skill,
        label: format!("skill `{}`", entry.item.name),
        file: entry.origin.display.clone(),
        text: entry.item.body.clone(),
        file_text: Some(entry.text.clone()),
    }
}

fn budget_finding(label: &str, file: &str, text: &str, budget: u64) -> Option<Finding> {
    let t = estimate_tokens(text);
    (t > budget).then(|| Finding {
        severity: Severity::Warning,
        rule: Rule::TokenBudget,
        loc: Loc {
            file: file.to_owned(),
            line: 0,
            column: 0,
        },
        message: format!("{label} is about {t} tokens, over the budget of {budget}"),
        suggestion: Some(
            "move rarely needed detail into a skill the model can read on demand".into(),
        ),
    })
}

/// Lints everything in the catalog plus caller-supplied `memory` sources.
///
/// Includes the catalog's load errors and, if `ctx` is given, agent
/// validation errors as [`Rule::Invalid`] errors.
pub fn lint_catalog(
    catalog: &Catalog,
    ctx: Option<&ValidationContext>,
    memory: &[LintSource],
    opts: &LintOptions,
) -> LintReport {
    let mut report = LintReport::default();
    let f = &mut report.findings;
    for e in catalog.errors() {
        f.push(invalid(&e.error, e.to_string()));
    }
    if let Some(ctx) = ctx {
        for e in catalog.validate_agents(ctx) {
            f.push(invalid(&e, e.message.clone()));
        }
    }
    // Every source once for blanket prohibitions and budgets.
    for p in catalog.prompts.active() {
        for s in prompt_sources(p) {
            f.extend(blanket_prohibitions(&s));
        }
        let label = format!("prompt `{}`", p.item.name);
        f.extend(budget_finding(
            &label,
            &p.origin.display,
            &p.item.raw_text(),
            opts.prompt_token_budget,
        ));
        // Contradictions between layers of one prompt.
        f.extend(contradictions(&prompt_sources(p)));
    }
    let skills = catalog.skills.active();
    let skill_srcs: Vec<LintSource> = skills.iter().map(|s| skill_source(s)).collect();
    for s in &skill_srcs {
        f.extend(blanket_prohibitions(s));
    }
    let listing = crate::skills::listing(skills.iter().map(|e| &e.item));
    f.extend(budget_finding(
        "skill listing",
        "skills",
        &listing,
        opts.prompt_token_budget,
    ));
    for m in memory {
        f.extend(blanket_prohibitions(m));
    }
    for a in catalog.agents.active() {
        let own = agent_sources(a, catalog);
        if let PromptRef::Inline(t) = &a.item.prompt {
            for s in &own {
                f.extend(blanket_prohibitions(s));
            }
            let label = format!("agent `{}` prompt_text", a.item.name);
            f.extend(budget_finding(
                &label,
                &a.origin.display,
                t,
                opts.prompt_token_budget,
            ));
        }
        f.extend(tool_contradictions(&a.item, &own));
        // Cross-source: prompt vs skills (model-invocable) vs memory.
        let invocable: Vec<LintSource> = skills
            .iter()
            .zip(&skill_srcs)
            .filter(|(e, _)| e.item.model_invocable)
            .map(|(_, s)| s.clone())
            .collect();
        let mut others = invocable;
        if a.item.memory != MemoryAccess::Off {
            others.extend(memory.iter().cloned());
        }
        let n_own = own.len();
        let inline = matches!(a.item.prompt, PromptRef::Inline(_));
        let mut all = own;
        all.extend(others);
        // Prompt-internal pairs are reported once per prompt above, so here
        // keep pairs involving a skill or memory (or an inline prompt_text).
        f.extend(contradictions_where(&all, |i, j| {
            inline || i >= n_own || j >= n_own
        }));
    }
    report.sort();
    report
}

fn invalid(e: &crate::yaml::YamlError, message: String) -> Finding {
    let msg_only = message
        .strip_prefix(&format!(
            "{}: ",
            Loc {
                file: e.source_name(),
                line: e.line,
                column: e.column
            }
        ))
        .unwrap_or(&message)
        .to_owned();
    Finding {
        severity: Severity::Error,
        rule: Rule::Invalid,
        loc: Loc {
            file: e.source_name(),
            line: e.line,
            column: e.column,
        },
        message: msg_only,
        suggestion: None,
    }
}

/// Entry point for `harness lint`: prints every finding and a summary.
/// Returns 1 if there are errors, else 0.
pub fn run_lint(report: &LintReport, out: &mut dyn Write) -> i32 {
    for f in &report.findings {
        let _ = writeln!(out, "{f}");
    }
    let _ = writeln!(out, "{}", report.summary());
    i32::from(report.has_errors())
}

/// Lines for `harness doctor`: every warning (errors are `harness lint`'s
/// job but are included too, so doctor never hides a problem).
pub fn doctor_lines(report: &LintReport) -> Vec<String> {
    report.findings.iter().map(ToString::to_string).collect()
}

#[cfg(test)]
mod tests;
