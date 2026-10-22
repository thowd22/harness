//! Conservative recursive-descent bash parser.
//!
//! Produces the flat list of simple commands in a command line. It does not
//! build a full syntax tree: control-flow keywords only separate commands,
//! because policy only needs to know which commands can run.

use super::{MAX_BRACE_WORDS, MAX_DEPTH, ParseError, Redirect, RedirectOp, SimpleCommand, Word};

/// Where a list ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Close {
    /// End of input.
    Eof,
    /// A `)` closing a subshell or substitution.
    Paren,
}

/// How words of the current command are interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Ordinary command words.
    Normal,
    /// Inside `[[ ... ]]`: operators are words.
    Test,
    /// After `for`/`select`: the loop variable and word list are not run.
    ForHeader,
}

/// One character of a word, and whether quoting made it literal.
#[derive(Debug, Clone, Copy)]
struct Ch {
    c: char,
    quoted: bool,
}

#[derive(Debug, Default)]
struct RawWord {
    chars: Vec<Ch>,
    dynamic: bool,
    had_quotes: bool,
}

impl RawWord {
    fn push(&mut self, c: char, quoted: bool) {
        self.chars.push(Ch { c, quoted });
    }

    fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            self.push(c, true);
        }
    }

    fn text(&self) -> String {
        self.chars.iter().map(|c| c.c).collect()
    }

    fn is_empty(&self) -> bool {
        self.chars.is_empty() && !self.had_quotes && !self.dynamic
    }
}

struct PendingHeredoc {
    delim: String,
    strip_tabs: bool,
    quoted: bool,
}

/// Parser state for one source string.
pub(super) struct Parser {
    chars: Vec<char>,
    pos: usize,
    depth: usize,
    heredocs: Vec<PendingHeredoc>,
    out: Vec<SimpleCommand>,
}

const RESERVED_SEPARATORS: &[&str] = &[
    "if", "then", "elif", "else", "fi", "do", "done", "while", "until", "{", "}", "!",
];

impl Parser {
    pub(super) fn new(src: &str, depth: usize) -> Self {
        Self {
            chars: src.chars().collect(),
            pos: 0,
            depth,
            heredocs: Vec::new(),
            out: Vec::new(),
        }
    }

    /// Parse everything and return the simple commands (not yet unwrapped).
    pub(super) fn run(mut self) -> Result<Vec<SimpleCommand>, ParseError> {
        if self.depth > MAX_DEPTH {
            return Err(ParseError::TooDeep);
        }
        self.parse_list(Close::Eof)?;
        if !self.heredocs.is_empty() {
            return Err(ParseError::Unterminated("here-document"));
        }
        Ok(self.out)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(i, c)| self.peek_at(i) == Some(c))
    }

    fn nested<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ParseError::TooDeep);
        }
        let r = f(self);
        self.depth -= 1;
        r
    }

    /// Skip spaces, tabs and line continuations (not newlines).
    fn skip_blanks(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t') => self.pos += 1,
                Some('\\') if self.peek_at(1) == Some('\n') => self.pos += 2,
                _ => return,
            }
        }
    }

    fn skip_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                return;
            }
            self.pos += 1;
        }
    }

    fn parse_list(&mut self, close: Close) -> Result<(), ParseError> {
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else {
                return match close {
                    Close::Eof => Ok(()),
                    Close::Paren => {
                        Err(ParseError::Unterminated("subshell or command substitution"))
                    }
                };
            };
            match c {
                ')' => {
                    return if close == Close::Paren {
                        self.pos += 1;
                        Ok(())
                    } else {
                        Err(ParseError::UnbalancedParen)
                    };
                }
                '\n' => {
                    self.pos += 1;
                    self.read_heredocs()?;
                }
                '#' => self.skip_comment(),
                ';' => {
                    if self.peek_at(1) == Some(';') {
                        return Err(ParseError::Unsupported("`;;` outside case".into()));
                    }
                    self.pos += 1;
                }
                '&' if self.peek_at(1) != Some('>') => {
                    self.pos += 1;
                    if self.peek() == Some('&') {
                        self.pos += 1;
                    }
                }
                '|' => {
                    self.pos += 1;
                    if matches!(self.peek(), Some('|' | '&')) {
                        self.pos += 1;
                    }
                }
                '(' => {
                    if self.peek_at(1) == Some('(') {
                        return Err(ParseError::Unsupported("arithmetic command `(( ))`".into()));
                    }
                    self.pos += 1;
                    self.nested(|p| p.parse_list(Close::Paren))?;
                }
                _ => self.parse_command()?,
            }
        }
    }

    fn parse_command(&mut self) -> Result<(), ParseError> {
        let mut cmd = SimpleCommand::default();
        let mut mode = Mode::Normal;
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { break };
            if mode == Mode::Test {
                if c == '\n' {
                    break;
                }
                let raw = self.read_word(true)?;
                let text = raw.text();
                let end = text == "]]" && !raw.had_quotes;
                cmd.words.push(Word {
                    text,
                    dynamic: raw.dynamic,
                });
                if end {
                    mode = Mode::Normal;
                }
                continue;
            }
            match c {
                '\n' | ';' | '|' | ')' => break,
                '&' if self.peek_at(1) != Some('>') => break,
                '#' => {
                    self.skip_comment();
                    break;
                }
                '(' => {
                    let function = cmd.words.len() == 1 && {
                        let save = self.pos;
                        self.pos += 1;
                        self.skip_blanks();
                        let close = self.peek() == Some(')');
                        self.pos = save;
                        close
                    };
                    return Err(ParseError::Unsupported(if function {
                        "function definitions".into()
                    } else {
                        "unexpected `(`".into()
                    }));
                }
                '<' | '>' if self.peek_at(1) == Some('(') => {
                    let marker = if c == '<' { "<(...)" } else { ">(...)" };
                    self.pos += 2;
                    self.nested(|p| p.parse_list(Close::Paren))?;
                    self.after_process_substitution(&mut cmd, mode, marker)?;
                }
                '<' | '>' | '&' => {
                    let r = self.redirect(None)?;
                    cmd.redirects.push(r);
                }
                _ => {
                    if let Some(fd) = self.fd_prefix() {
                        let r = self.redirect(Some(fd))?;
                        cmd.redirects.push(r);
                        continue;
                    }
                    let raw = self.read_word(false)?;
                    if raw.is_empty() {
                        // Defensive: never loop without consuming input.
                        return Err(ParseError::Unsupported(format!("unexpected `{c}`")));
                    }
                    self.handle_word(&mut cmd, raw, &mut mode)?;
                }
            }
        }
        if !(cmd.words.is_empty() && cmd.assignments.is_empty() && cmd.redirects.is_empty()) {
            self.out.push(cmd);
        }
        Ok(())
    }

    fn after_process_substitution(
        &mut self,
        cmd: &mut SimpleCommand,
        mode: Mode,
        marker: &str,
    ) -> Result<(), ParseError> {
        if mode == Mode::Normal {
            cmd.words.push(Word {
                text: marker.into(),
                dynamic: true,
            });
        }
        Ok(())
    }

    /// Digits immediately followed by `<` or `>` form a descriptor prefix.
    fn fd_prefix(&mut self) -> Option<u32> {
        let mut j = self.pos;
        while self.chars.get(j).is_some_and(char::is_ascii_digit) {
            j += 1;
        }
        if j == self.pos
            || !matches!(self.chars.get(j), Some('<' | '>'))
            || self.chars.get(j + 1) == Some(&'(')
        {
            return None;
        }
        let fd: String = self.chars[self.pos..j].iter().collect();
        let fd = fd.parse().ok()?;
        self.pos = j;
        Some(fd)
    }

    fn handle_word(
        &mut self,
        cmd: &mut SimpleCommand,
        raw: RawWord,
        mode: &mut Mode,
    ) -> Result<(), ParseError> {
        if *mode == Mode::ForHeader {
            // Loop variable and word list: substitutions inside them were
            // already recorded; the words themselves do not run.
            return Ok(());
        }
        if cmd.words.is_empty() {
            if !raw.had_quotes && !raw.dynamic {
                let text = raw.text();
                if RESERVED_SEPARATORS.contains(&text.as_str()) {
                    return Ok(());
                }
                match text.as_str() {
                    "for" | "select" => {
                        self.skip_blanks();
                        if self.starts_with("((") {
                            return Err(ParseError::Unsupported(
                                "arithmetic `for ((...))` loops".into(),
                            ));
                        }
                        *mode = Mode::ForHeader;
                        return Ok(());
                    }
                    "case" | "function" | "coproc" => {
                        return Err(ParseError::Unsupported(format!("`{text}`")));
                    }
                    "[[" => {
                        *mode = Mode::Test;
                        cmd.words.push(Word::literal(text));
                        return Ok(());
                    }
                    _ => {}
                }
            }
            if let Some(split) = assignment_split(&raw.chars) {
                let name: String = raw.chars[..split.name_end].iter().map(|c| c.c).collect();
                let value: String = raw.chars[split.value_start..].iter().map(|c| c.c).collect();
                if raw.chars.get(split.value_start).is_none() && self.peek() == Some('(') {
                    return Err(ParseError::Unsupported("array assignments".into()));
                }
                cmd.assignments.push((
                    name,
                    Word {
                        text: value,
                        dynamic: raw.dynamic,
                    },
                ));
                return Ok(());
            }
        }
        for text in brace_expand(&raw.chars)? {
            cmd.words.push(Word {
                text,
                dynamic: raw.dynamic,
            });
        }
        Ok(())
    }

    fn redirect(&mut self, fd: Option<u32>) -> Result<Redirect, ParseError> {
        let ops: &[(&str, RedirectOp)] = &[
            ("&>>", RedirectOp::AppendAll),
            ("&>", RedirectOp::OutputAll),
            ("<<<", RedirectOp::HereString),
            ("<<-", RedirectOp::HereDoc),
            ("<<", RedirectOp::HereDoc),
            ("<>", RedirectOp::ReadWrite),
            ("<&", RedirectOp::Duplicate),
            ("<", RedirectOp::Input),
            (">>", RedirectOp::Append),
            (">|", RedirectOp::Clobber),
            (">&", RedirectOp::Duplicate),
            (">", RedirectOp::Output),
        ];
        let (tok, mut op) = ops
            .iter()
            .copied()
            .find(|(t, _)| self.starts_with(t))
            .ok_or_else(|| ParseError::Unsupported("unexpected `&`".into()))?;
        self.pos += tok.chars().count();
        self.skip_blanks();
        let raw = self.read_word(false)?;
        if raw.is_empty() {
            return Err(ParseError::Unsupported("redirect without a target".into()));
        }
        let text = raw.text();
        if tok == ">&" && !is_descriptor(&text) {
            op = RedirectOp::OutputAll;
        }
        if op == RedirectOp::HereDoc {
            self.heredocs.push(PendingHeredoc {
                delim: text.clone(),
                strip_tabs: tok == "<<-",
                quoted: raw.had_quotes,
            });
        }
        Ok(Redirect {
            fd,
            op,
            target: Word {
                text,
                dynamic: raw.dynamic,
            },
        })
    }

    fn read_heredocs(&mut self) -> Result<(), ParseError> {
        for hd in std::mem::take(&mut self.heredocs) {
            loop {
                if self.pos >= self.chars.len() {
                    return Err(ParseError::Unterminated("here-document"));
                }
                let start = self.pos;
                while self.peek().is_some_and(|c| c != '\n') {
                    self.pos += 1;
                }
                let line: String = self.chars[start..self.pos].iter().collect();
                if self.peek() == Some('\n') {
                    self.pos += 1;
                }
                let cmp = if hd.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    &line
                };
                if cmp == hd.delim {
                    break;
                }
                if !hd.quoted && (line.contains("$(") || line.contains('`')) {
                    return Err(ParseError::Unsupported(
                        "command substitution inside an unquoted here-document".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Read one word. In `test` mode (inside `[[ ]]`) a leading operator is
    /// returned as its own word.
    fn read_word(&mut self, test: bool) -> Result<RawWord, ParseError> {
        let mut w = RawWord::default();
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\n' => break,
                ';' | '&' | '|' | '<' | '>' | '(' | ')' => {
                    if test && w.chars.is_empty() {
                        w.push(c, false);
                        self.pos += 1;
                        if matches!((c, self.peek()), ('&', Some('&')) | ('|', Some('|'))) {
                            w.push(c, false);
                            self.pos += 1;
                        }
                    }
                    break;
                }
                '\\' => {
                    match self.peek_at(1) {
                        Some('\n') => {}
                        Some(n) => w.push(n, true),
                        None => w.push('\\', true),
                    }
                    w.had_quotes = true;
                    self.pos = (self.pos + 2).min(self.chars.len());
                }
                '\'' => {
                    self.pos += 1;
                    loop {
                        match self.peek() {
                            None => return Err(ParseError::Unterminated("single quote")),
                            Some('\'') => break,
                            Some(q) => w.push(q, true),
                        }
                        self.pos += 1;
                    }
                    self.pos += 1;
                    w.had_quotes = true;
                }
                '"' => {
                    self.pos += 1;
                    self.read_double_quoted(&mut w)?;
                    w.had_quotes = true;
                }
                '`' => self.read_backtick(&mut w, false)?,
                '$' => self.read_dollar(&mut w, false)?,
                _ => {
                    w.push(c, false);
                    self.pos += 1;
                }
            }
        }
        Ok(w)
    }

    fn read_double_quoted(&mut self, w: &mut RawWord) -> Result<(), ParseError> {
        loop {
            match self.peek() {
                None => return Err(ParseError::Unterminated("double quote")),
                Some('"') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some('\\') => match self.peek_at(1) {
                    Some(n @ ('$' | '`' | '"' | '\\')) => {
                        w.push(n, true);
                        self.pos += 2;
                    }
                    Some('\n') => self.pos += 2,
                    _ => {
                        w.push('\\', true);
                        self.pos += 1;
                    }
                },
                Some('$') => self.read_dollar(w, true)?,
                Some('`') => self.read_backtick(w, true)?,
                Some(c) => {
                    w.push(c, true);
                    self.pos += 1;
                }
            }
        }
    }

    fn read_backtick(&mut self, w: &mut RawWord, in_dq: bool) -> Result<(), ParseError> {
        self.pos += 1;
        let mut inner = String::new();
        loop {
            match self.peek() {
                None => return Err(ParseError::Unterminated("backtick substitution")),
                Some('`') => break,
                Some('\\') => match self.peek_at(1) {
                    Some(n) if matches!(n, '`' | '\\' | '$') || (in_dq && n == '"') => {
                        inner.push(n);
                        self.pos += 2;
                    }
                    _ => {
                        inner.push('\\');
                        self.pos += 1;
                    }
                },
                Some(c) => {
                    inner.push(c);
                    self.pos += 1;
                }
            }
        }
        self.pos += 1;
        let cmds = Parser::new(&inner, self.depth + 1).run()?;
        self.out.extend(cmds);
        w.push_str("`...`");
        w.dynamic = true;
        Ok(())
    }

    fn read_dollar(&mut self, w: &mut RawWord, in_dq: bool) -> Result<(), ParseError> {
        match self.peek_at(1) {
            Some('(') if self.peek_at(2) == Some('(') => self.read_arithmetic(w),
            Some('(') => {
                self.pos += 2;
                self.nested(|p| p.parse_list(Close::Paren))?;
                w.push_str("$(...)");
                w.dynamic = true;
                Ok(())
            }
            Some('{') => {
                let start = self.pos;
                self.pos += 2;
                let mut depth = 1;
                while depth > 0 {
                    match self.peek() {
                        None => return Err(ParseError::Unterminated("`${`")),
                        Some('{') => depth += 1,
                        Some('}') => depth -= 1,
                        Some('`') => {
                            return Err(ParseError::Unsupported(
                                "substitution inside `${...}`".into(),
                            ));
                        }
                        Some('$') if self.peek_at(1) == Some('(') => {
                            return Err(ParseError::Unsupported(
                                "substitution inside `${...}`".into(),
                            ));
                        }
                        _ => {}
                    }
                    self.pos += 1;
                }
                let text: String = self.chars[start..self.pos].iter().collect();
                w.push_str(&text);
                w.dynamic = true;
                Ok(())
            }
            Some('\'') if !in_dq => {
                self.pos += 2;
                let s = self.read_ansi_c()?;
                w.push_str(&s);
                w.had_quotes = true;
                Ok(())
            }
            Some('"') if !in_dq => {
                // `$"..."` is a locale-translated string: treat as "...".
                self.pos += 1;
                Ok(())
            }
            Some(c) if c.is_ascii_digit() || "@*#?-$!".contains(c) => {
                w.push_str(&format!("${c}"));
                w.dynamic = true;
                self.pos += 2;
                Ok(())
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let start = self.pos;
                self.pos += 1;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    self.pos += 1;
                }
                let text: String = self.chars[start..self.pos].iter().collect();
                w.push_str(&text);
                w.dynamic = true;
                Ok(())
            }
            _ => {
                w.push('$', in_dq);
                self.pos += 1;
                Ok(())
            }
        }
    }

    fn read_arithmetic(&mut self, w: &mut RawWord) -> Result<(), ParseError> {
        let start = self.pos;
        self.pos += 3;
        let mut depth = 0usize;
        loop {
            match self.peek() {
                None => return Err(ParseError::Unterminated("`$((`")),
                Some('(') => depth += 1,
                Some(')') if depth == 0 => {
                    if self.peek_at(1) != Some(')') {
                        return Err(ParseError::Unsupported("ambiguous `$((`".into()));
                    }
                    self.pos += 2;
                    break;
                }
                Some(')') => depth -= 1,
                Some('`') => {
                    return Err(ParseError::Unsupported(
                        "substitution inside arithmetic".into(),
                    ));
                }
                Some('$') if self.peek_at(1) == Some('(') => {
                    return Err(ParseError::Unsupported(
                        "substitution inside arithmetic".into(),
                    ));
                }
                _ => {}
            }
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        w.push_str(&text);
        w.dynamic = true;
        Ok(())
    }

    /// Decode the body of `$'...'` (after the opening quote).
    fn read_ansi_c(&mut self) -> Result<String, ParseError> {
        let mut s = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(ParseError::Unterminated("`$'` string"));
            };
            self.pos += 1;
            match c {
                '\'' => return Ok(s),
                '\\' => {
                    let Some(e) = self.peek() else {
                        return Err(ParseError::Unterminated("`$'` string"));
                    };
                    self.pos += 1;
                    match e {
                        'a' => s.push('\x07'),
                        'b' => s.push('\x08'),
                        'e' | 'E' => s.push('\x1b'),
                        'f' => s.push('\x0c'),
                        'n' => s.push('\n'),
                        'r' => s.push('\r'),
                        't' => s.push('\t'),
                        'v' => s.push('\x0b'),
                        '\\' | '\'' | '"' | '?' => s.push(e),
                        '0'..='7' => {
                            let v = self.read_radix(8, 2, e.to_digit(8).unwrap_or(0));
                            s.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                        }
                        'x' => s.push(self.read_escape(16, 2)?),
                        'u' => s.push(self.read_escape(16, 4)?),
                        'U' => s.push(self.read_escape(16, 8)?),
                        'c' => {
                            return Err(ParseError::Unsupported(
                                "`\\c` escapes in `$'...'`".into(),
                            ));
                        }
                        other => {
                            s.push('\\');
                            s.push(other);
                        }
                    }
                }
                c => s.push(c),
            }
        }
    }

    fn read_radix(&mut self, radix: u32, max: usize, mut v: u32) -> u32 {
        for _ in 0..max {
            match self.peek().and_then(|c| c.to_digit(radix)) {
                Some(d) => {
                    v = v * radix + d;
                    self.pos += 1;
                }
                None => break,
            }
        }
        v
    }

    fn read_escape(&mut self, radix: u32, max: usize) -> Result<char, ParseError> {
        if self.peek().and_then(|c| c.to_digit(radix)).is_none() {
            return Err(ParseError::Unsupported("empty escape in `$'...'`".into()));
        }
        let v = self.read_radix(radix, max, 0);
        char::from_u32(v)
            .ok_or_else(|| ParseError::Unsupported("invalid escape in `$'...'`".into()))
    }
}

fn is_descriptor(s: &str) -> bool {
    let s = s.strip_suffix('-').unwrap_or(s);
    s.is_empty() || s.chars().all(|c| c.is_ascii_digit())
}

struct AssignmentSplit {
    name_end: usize,
    value_start: usize,
}

/// `NAME=value` or `NAME+=value` with an unquoted name and `=`.
fn assignment_split(chars: &[Ch]) -> Option<AssignmentSplit> {
    let first = chars.first()?;
    if first.quoted || !(first.c.is_ascii_alphabetic() || first.c == '_') {
        return None;
    }
    let mut i = 1;
    while chars
        .get(i)
        .is_some_and(|c| !c.quoted && (c.c.is_ascii_alphanumeric() || c.c == '_'))
    {
        i += 1;
    }
    let name_end = i;
    if chars.get(i).is_some_and(|c| !c.quoted && c.c == '+') {
        i += 1;
    }
    if chars.get(i).is_some_and(|c| !c.quoted && c.c == '=') {
        Some(AssignmentSplit {
            name_end,
            value_start: i + 1,
        })
    } else {
        None
    }
}

/// Bash brace expansion over unquoted `{a,b}` and `{x..y}` groups.
fn brace_expand(chars: &[Ch]) -> Result<Vec<String>, ParseError> {
    let mut out = Vec::new();
    expand_into(chars, &mut out)?;
    Ok(out)
}

fn expand_into(chars: &[Ch], out: &mut Vec<String>) -> Result<(), ParseError> {
    let Some((open, close, alts)) = find_brace_group(chars)? else {
        out.push(chars.iter().map(|c| c.c).collect());
        return Ok(());
    };
    for alt in alts {
        let mut word: Vec<Ch> = chars[..open].to_vec();
        word.extend(alt);
        word.extend_from_slice(&chars[close + 1..]);
        expand_into(&word, out)?;
        if out.len() > MAX_BRACE_WORDS {
            return Err(ParseError::Unsupported(
                "brace expansion produces too many words".into(),
            ));
        }
    }
    Ok(())
}

type BraceGroup = (usize, usize, Vec<Vec<Ch>>);

fn find_brace_group(chars: &[Ch]) -> Result<Option<BraceGroup>, ParseError> {
    for open in 0..chars.len() {
        if chars[open].quoted || chars[open].c != '{' {
            continue;
        }
        let mut depth = 0usize;
        let mut commas = Vec::new();
        let mut close = None;
        for (i, ch) in chars.iter().enumerate().skip(open + 1) {
            if ch.quoted {
                continue;
            }
            match ch.c {
                '{' => depth += 1,
                '}' if depth == 0 => {
                    close = Some(i);
                    break;
                }
                '}' => depth -= 1,
                ',' if depth == 0 => commas.push(i),
                _ => {}
            }
        }
        let Some(close) = close else { continue };
        if !commas.is_empty() {
            let mut alts = Vec::new();
            let mut start = open + 1;
            for &c in commas.iter().chain(std::iter::once(&close)) {
                alts.push(chars[start..c].to_vec());
                start = c + 1;
            }
            return Ok(Some((open, close, alts)));
        }
        let body: String = chars[open + 1..close].iter().map(|c| c.c).collect();
        if let Some(seq) = brace_range(&body)? {
            let alts = seq
                .into_iter()
                .map(|s| s.chars().map(|c| Ch { c, quoted: true }).collect())
                .collect();
            return Ok(Some((open, close, alts)));
        }
    }
    Ok(None)
}

/// `{1..5}`, `{a..e}`, `{1..10..2}`.
fn brace_range(body: &str) -> Result<Option<Vec<String>>, ParseError> {
    let parts: Vec<&str> = body.split("..").collect();
    if !(parts.len() == 2 || parts.len() == 3) {
        return Ok(None);
    }
    let step: i64 = match parts.get(2) {
        Some(s) => match s.parse::<i64>() {
            Ok(v) if v != 0 => v.abs(),
            _ => return Ok(None),
        },
        None => 1,
    };
    let too_many = || ParseError::Unsupported("brace expansion produces too many words".into());
    if let (Ok(a), Ok(b)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) {
        let count = (a - b).unsigned_abs() / step.unsigned_abs() + 1;
        if count > MAX_BRACE_WORDS as u64 {
            return Err(too_many());
        }
        let dir = if b >= a { step } else { -step };
        return Ok(Some(
            (0..count as i64)
                .map(|i| (a + i * dir).to_string())
                .collect(),
        ));
    }
    let single = |s: &str| {
        let mut it = s.chars();
        match (it.next(), it.next()) {
            (Some(c), None) if c.is_ascii_alphabetic() => Some(c as i64),
            _ => None,
        }
    };
    if let (Some(a), Some(b)) = (single(parts[0]), single(parts[1])) {
        let count = (a - b).unsigned_abs() / step.unsigned_abs() + 1;
        let dir = if b >= a { step } else { -step };
        return Ok(Some(
            (0..count as i64)
                .filter_map(|i| char::from_u32((a + i * dir) as u32).map(String::from))
                .collect(),
        ));
    }
    Ok(None)
}
