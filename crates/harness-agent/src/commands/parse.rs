//! Argument specs, tokenizer and binding.

use std::collections::BTreeMap;

use super::registry::CommandSpec;

/// How an argument is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// By position (or as `name=value`).
    Positional,
    /// Only as `name=value`.
    Named,
    /// Everything that is left, as raw text (e.g. `/compact <focus...>`).
    Rest,
}

/// One argument of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgSpec {
    /// Name (also the `key` of `key=value`).
    pub name: String,
    /// Refused when missing.
    pub required: bool,
    /// Short help, e.g. `id, prefix or last`.
    pub help: Option<String>,
    /// How it is given.
    pub kind: ArgKind,
    /// Allowed values; empty = any.
    pub choices: Vec<String>,
    /// Value used when missing.
    pub default: Option<String>,
}

impl ArgSpec {
    /// An optional positional argument.
    pub fn optional(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            required: false,
            help: None,
            kind: ArgKind::Positional,
            choices: Vec::new(),
            default: None,
        }
    }

    /// A required positional argument.
    pub fn required(name: impl Into<String>) -> Self {
        Self {
            required: true,
            ..Self::optional(name)
        }
    }

    /// An optional rest-of-line argument.
    pub fn rest(name: impl Into<String>) -> Self {
        Self {
            kind: ArgKind::Rest,
            ..Self::optional(name)
        }
    }

    /// An optional `name=value` argument.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            kind: ArgKind::Named,
            ..Self::optional(name)
        }
    }

    /// Sets the help text.
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Restricts the values.
    pub fn with_choices(mut self, choices: &[&str]) -> Self {
        self.choices = choices.iter().map(|c| (*c).to_owned()).collect();
        self
    }

    /// Sets the default.
    pub fn with_default(mut self, default: impl Into<String>) -> Self {
        self.default = Some(default.into());
        self
    }

    /// The usage fragment: `<name>`, `[name]`, `[a|b]`, `[name...]` or
    /// `[name=…]`.
    pub fn usage(&self) -> String {
        let body = if self.choices.is_empty() {
            self.name.clone()
        } else {
            self.choices.join("|")
        };
        let body = match self.kind {
            ArgKind::Rest => format!("{body}..."),
            ArgKind::Named => format!("{}=…", self.name),
            ArgKind::Positional => body,
        };
        if self.required {
            format!("<{body}>")
        } else {
            format!("[{body}]")
        }
    }

    /// One help line: `name: help (one of a, b; default x; required)`.
    pub fn help_line(&self) -> String {
        let mut notes = Vec::new();
        if !self.choices.is_empty() {
            notes.push(format!("one of {}", self.choices.join(", ")));
        }
        if let Some(d) = &self.default {
            notes.push(format!("default {d}"));
        }
        notes.push(
            if self.required {
                "required"
            } else {
                "optional"
            }
            .to_owned(),
        );
        match &self.help {
            Some(h) => format!("{}: {h} ({})", self.name, notes.join("; ")),
            None => format!("{} ({})", self.name, notes.join("; ")),
        }
    }
}

/// One word of an argument string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// `key` of `key=value`.
    pub key: Option<String>,
    /// The (unquoted) value.
    pub value: String,
    /// Byte offset of the word in the argument string.
    pub start: usize,
    /// Byte offset just past the word.
    pub end: usize,
}

/// Why a command line did not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    /// The line does not start with `/`.
    #[error("not a command: commands start with `/`")]
    NotACommand,
    /// No such command.
    #[error("unknown command `/{name}`.{} Type /help to list commands.", suggestion.as_ref().map(|s| format!(" Did you mean `/{s}`?")).unwrap_or_default())]
    Unknown {
        /// What was typed.
        name: String,
        /// The closest known name.
        suggestion: Option<String>,
    },
    /// A quote was not closed.
    #[error("unterminated {quote} quote starting at column {column}: close it with {quote}")]
    Unterminated {
        /// `'` or `"`.
        quote: char,
        /// 1-based column in the argument string.
        column: usize,
    },
    /// A required argument is missing.
    #[error("`/{command}` needs <{arg}>. Usage: {usage}")]
    MissingArg {
        /// Command.
        command: String,
        /// Argument.
        arg: String,
        /// Usage line.
        usage: String,
    },
    /// More positional arguments than the command takes.
    #[error("`/{command}` takes at most {max} argument(s), got `{extra}`. Usage: {usage}")]
    TooMany {
        /// Command.
        command: String,
        /// Maximum.
        max: usize,
        /// The first extra word.
        extra: String,
        /// Usage line.
        usage: String,
    },
    /// An unknown `key=`.
    #[error("`/{command}` has no argument `{key}`{}. Usage: {usage}", if known.is_empty() { String::new() } else { format!(" (known: {})", known.join(", ")) })]
    UnknownKey {
        /// Command.
        command: String,
        /// The key.
        key: String,
        /// Valid keys.
        known: Vec<String>,
        /// Usage line.
        usage: String,
    },
    /// A value outside the allowed choices.
    #[error("`{value}` is not a valid {arg} for `/{command}`: use one of {}", choices.join(", "))]
    InvalidChoice {
        /// Command.
        command: String,
        /// Argument.
        arg: String,
        /// The value.
        value: String,
        /// Allowed values.
        choices: Vec<String>,
    },
    /// The same argument was given twice.
    #[error("`/{command}` got {arg} twice. Usage: {usage}")]
    Duplicate {
        /// Command.
        command: String,
        /// Argument.
        arg: String,
        /// Usage line.
        usage: String,
    },
}

fn is_key(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Splits an argument string into words. Single quotes are literal; double
/// quotes allow `\"` and `\\`; `key=value` (with an optionally quoted value)
/// becomes a keyed token. A quote inside a word continues the word
/// (`a"b c"` is `ab c`).
pub fn tokenize(args: &str) -> Result<Vec<Token>, CommandError> {
    let mut out = Vec::new();
    let bytes: Vec<(usize, char)> = args.char_indices().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].1.is_whitespace() {
            i += 1;
            continue;
        }
        let start = bytes[i].0;
        let mut word = String::new();
        let mut key: Option<String> = None;
        let mut quoted_any = false;
        while i < bytes.len() && !bytes[i].1.is_whitespace() {
            let (pos, c) = bytes[i];
            match c {
                '\'' | '"' => {
                    quoted_any = true;
                    let quote = c;
                    i += 1;
                    let mut closed = false;
                    while i < bytes.len() {
                        let (_, d) = bytes[i];
                        if d == quote {
                            closed = true;
                            i += 1;
                            break;
                        }
                        if quote == '"' && d == '\\' && i + 1 < bytes.len() {
                            let next = bytes[i + 1].1;
                            if next == '"' || next == '\\' {
                                word.push(next);
                                i += 2;
                                continue;
                            }
                        }
                        word.push(d);
                        i += 1;
                    }
                    if !closed {
                        return Err(CommandError::Unterminated {
                            quote,
                            column: args[..pos].chars().count() + 1,
                        });
                    }
                }
                '=' if key.is_none() && !quoted_any && is_key(&word) => {
                    key = Some(std::mem::take(&mut word));
                    i += 1;
                }
                _ => {
                    word.push(c);
                    i += 1;
                }
            }
        }
        let end = bytes.get(i).map_or(args.len(), |b| b.0);
        out.push(Token {
            key,
            value: word,
            start,
            end,
        });
    }
    Ok(out)
}

/// Splits `/name rest` into `("name", "rest")`; `None` without a leading
/// slash.
pub fn split_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    let body = line.strip_prefix('/')?;
    let end = body.find(char::is_whitespace).unwrap_or(body.len());
    Some((&body[..end], body[end..].trim()))
}

/// A parsed command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    /// The name as typed (may be scope-qualified).
    pub name: String,
    /// The command.
    pub spec: CommandSpec,
    /// Values by argument name (and extra `key=value` pairs of lenient
    /// commands), defaults applied.
    pub args: BTreeMap<String, String>,
    /// Positional words that no argument took (lenient commands only).
    pub extra: Vec<String>,
    /// The raw argument string (what a skill's `$ARGUMENTS` expands to).
    pub raw: String,
}

impl Invocation {
    /// The value of `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.args.get(name).map(String::as_str)
    }
}

/// Binds `raw` arguments to `spec`.
pub fn bind(name: &str, spec: &CommandSpec, raw: &str) -> Result<Invocation, CommandError> {
    let tokens = tokenize(raw)?;
    let usage = spec.usage();
    let command = spec.name.clone();
    let mut args: BTreeMap<String, String> = BTreeMap::new();
    let mut extra = Vec::new();
    let positional: Vec<&ArgSpec> = spec
        .args
        .iter()
        .filter(|a| a.kind == ArgKind::Positional)
        .collect();
    let rest = spec.args.iter().find(|a| a.kind == ArgKind::Rest);
    let mut next = 0;
    for t in &tokens {
        if let Some(key) = &t.key {
            let known = spec.args.iter().any(|a| a.name == *key);
            if !known && spec.strict {
                return Err(CommandError::UnknownKey {
                    command,
                    key: key.clone(),
                    known: spec.args.iter().map(|a| a.name.clone()).collect(),
                    usage,
                });
            }
            if args.insert(key.clone(), t.value.clone()).is_some() {
                return Err(CommandError::Duplicate {
                    command,
                    arg: key.clone(),
                    usage,
                });
            }
            continue;
        }
        while next < positional.len() && args.contains_key(&positional[next].name) {
            next += 1;
        }
        if next < positional.len() {
            args.insert(positional[next].name.clone(), t.value.clone());
            next += 1;
        } else if let Some(r) = rest {
            // The rest is raw text from here on, `key=value` words included.
            args.insert(r.name.clone(), raw[t.start..].trim().to_owned());
            break;
        } else if spec.strict {
            return Err(CommandError::TooMany {
                command,
                max: positional.len(),
                extra: t.value.clone(),
                usage,
            });
        } else {
            extra.push(t.value.clone());
        }
    }
    for a in &spec.args {
        match args.get(&a.name) {
            Some(v) if !a.choices.is_empty() && !a.choices.iter().any(|c| c == v) => {
                return Err(CommandError::InvalidChoice {
                    command,
                    arg: a.name.clone(),
                    value: v.clone(),
                    choices: a.choices.clone(),
                });
            }
            Some(_) => {}
            None => match &a.default {
                Some(d) => {
                    args.insert(a.name.clone(), d.clone());
                }
                None if a.required => {
                    return Err(CommandError::MissingArg {
                        command,
                        arg: a.name.clone(),
                        usage,
                    });
                }
                None => {}
            },
        }
    }
    Ok(Invocation {
        name: name.to_owned(),
        spec: spec.clone(),
        args,
        extra,
        raw: raw.trim().to_owned(),
    })
}
