//! Shell command parsing for policy decisions.
//!
//! Matching policy patterns against a raw command string is easy to bypass
//! with pipes, subshells, chained commands or quoting (`g''it push`). This
//! module turns a bash command line into a flat list of [`SimpleCommand`]s so
//! that every command that would run can be checked on its own.
//!
//! The parser is deliberately conservative: it understands the shapes agents
//! actually write (lists, pipelines, subshells, groups, `if`/`for`/`while`
//! bodies, command and process substitution, env prefixes, redirects,
//! here-documents, brace expansion and the common quoting forms) and returns a
//! [`ParseError`] for anything else. Callers must treat an error as "cannot
//! tell what this runs" and fall back to asking the user.
//!
//! Commands that run other commands (`env`, `sudo`, `xargs`, `bash -c`,
//! `eval`, `find -exec`, ...) are unwrapped: the outer command is kept and
//! marked [`SimpleCommand::wrapper`], and the inner command is added as its
//! own entry. See `docs/policy.md` for the parser spike and the full list.

mod parser;
mod wrappers;

use std::fmt;

pub use wrappers::WRAPPERS;

/// Maximum nesting of subshells, substitutions and `bash -c` strings.
pub const MAX_DEPTH: usize = 32;

/// Maximum number of words one brace expansion may produce.
pub const MAX_BRACE_WORDS: usize = 256;

/// A word after quote removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// The literal text. Parameter expansions are kept verbatim (`$HOME`),
    /// command substitutions are replaced by a placeholder (`$(...)`).
    pub text: String,
    /// True when the word's value is only known at run time (it contains a
    /// parameter expansion, command/process substitution or arithmetic).
    pub dynamic: bool,
}

impl Word {
    /// A static word.
    pub fn literal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            dynamic: false,
        }
    }
}

/// Redirection operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectOp {
    /// `<`
    Input,
    /// `>`
    Output,
    /// `>|`
    Clobber,
    /// `>>`
    Append,
    /// `<>`
    ReadWrite,
    /// `&>` or `>&file`
    OutputAll,
    /// `&>>`
    AppendAll,
    /// `<&n` / `>&n`: duplicates a descriptor, the target is not a path.
    Duplicate,
    /// `<<<`
    HereString,
    /// `<<` / `<<-`
    HereDoc,
}

impl RedirectOp {
    /// True if the target names a file the shell opens.
    pub fn targets_path(self) -> bool {
        !matches!(self, Self::Duplicate | Self::HereString | Self::HereDoc)
    }

    /// True if the redirect writes to its target.
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::Output
                | Self::Clobber
                | Self::Append
                | Self::ReadWrite
                | Self::OutputAll
                | Self::AppendAll
        )
    }
}

/// One redirection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    /// Explicit file descriptor (`2>`), if any.
    pub fd: Option<u32>,
    /// The operator.
    pub op: RedirectOp,
    /// Target word (file, descriptor or here-doc delimiter).
    pub target: Word,
}

/// A simple command: optional `NAME=value` prefixes, words and redirects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimpleCommand {
    /// Environment prefixes (`FOO=1 cmd`).
    pub assignments: Vec<(String, Word)>,
    /// Program and arguments. May be empty for a bare redirect (`> file`).
    pub words: Vec<Word>,
    /// Redirects attached to the command.
    pub redirects: Vec<Redirect>,
    /// True if this command only runs another command that is listed
    /// separately (`sudo`, `env`, `bash -c`, ...).
    pub wrapper: bool,
}

impl SimpleCommand {
    /// The program word as written, if any.
    pub fn program(&self) -> Option<&str> {
        self.words.first().map(|w| w.text.as_str())
    }

    /// The program's file name (`/usr/bin/git` becomes `git`).
    pub fn program_name(&self) -> Option<&str> {
        self.program().map(basename)
    }

    /// Argument words (everything after the program).
    pub fn args(&self) -> &[Word] {
        self.words.get(1..).unwrap_or(&[])
    }

    /// True if any word or assignment value is only known at run time.
    pub fn is_dynamic(&self) -> bool {
        self.words.iter().any(|w| w.dynamic)
            || self.assignments.iter().any(|(_, w)| w.dynamic)
            || self.redirects.iter().any(|r| r.target.dynamic)
    }

    /// The command line as policy patterns see it: words joined by one
    /// space, with the program as written.
    pub fn line(&self) -> String {
        join(self.words.iter().map(|w| w.text.as_str()))
    }

    /// Distinct forms patterns are matched against: the line as written and
    /// the line with the program reduced to its file name.
    pub fn forms(&self) -> Vec<String> {
        let line = self.line();
        let mut out = vec![line.clone()];
        if let (Some(p), Some(n)) = (self.program(), self.program_name())
            && p != n
        {
            out.push(join(
                std::iter::once(n).chain(self.args().iter().map(|w| w.text.as_str())),
            ));
        }
        out
    }

    /// Strings that may name files: arguments (and the value part of
    /// `--opt=value`), assignment values and redirect targets.
    pub fn path_candidates(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for w in self.args() {
            out.push(w.text.as_str());
            if let Some((_, v)) = w.text.split_once('=')
                && !v.is_empty()
            {
                out.push(v);
            }
        }
        out.extend(self.assignments.iter().map(|(_, w)| w.text.as_str()));
        out.extend(
            self.redirects
                .iter()
                .filter(|r| r.op.targets_path())
                .map(|r| r.target.text.as_str()),
        );
        out
    }
}

impl fmt::Display for SimpleCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.line())
    }
}

/// The parsed form of a command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Script {
    /// Every simple command that may run, in source order, with unwrapped
    /// inner commands following their wrapper.
    pub commands: Vec<SimpleCommand>,
}

/// Why a command line could not be understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// A quote, substitution or here-document is not closed.
    #[error("unterminated {0}")]
    Unterminated(&'static str),
    /// Parentheses do not balance.
    #[error("unbalanced parenthesis")]
    UnbalancedParen,
    /// Syntax the policy parser does not analyse (functions, `case`, ...).
    #[error("unsupported shell syntax: {0}")]
    Unsupported(String),
    /// The program name is computed at run time (`$CMD args`).
    #[error("the program name is computed at run time: {0}")]
    DynamicCommand(String),
    /// Too much nesting.
    #[error("command nests deeper than {MAX_DEPTH} levels")]
    TooDeep,
}

/// Parse a bash command line into the simple commands it runs.
pub fn parse(src: &str) -> Result<Script, ParseError> {
    parse_at_depth(src, 0)
}

pub(crate) fn parse_at_depth(src: &str, depth: usize) -> Result<Script, ParseError> {
    if depth > MAX_DEPTH {
        return Err(ParseError::TooDeep);
    }
    let commands = parser::Parser::new(src, depth).run()?;
    let mut out = Vec::with_capacity(commands.len());
    for cmd in commands {
        wrappers::push_unwrapped(cmd, depth, &mut out)?;
    }
    Ok(Script { commands: out })
}

/// The file-name part of a program path.
pub fn basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

fn join<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut s = String::new();
    for p in parts {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(p);
    }
    s
}

#[cfg(test)]
mod tests;
