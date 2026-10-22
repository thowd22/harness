//! A small, dependency-free syntax highlighter for code blocks and diffs.
//!
//! The language table is built lazily on the first highlighted block
//! ([`languages`] uses a `OnceLock`), and the transcript only highlights code
//! that is actually laid out on screen, so long sessions never pay for code
//! the user does not look at. Highlighting is lexical (keywords, strings,
//! comments, numbers, type-like identifiers), which is enough to make code
//! scannable without a grammar engine.

use std::sync::OnceLock;

use ratatui::style::Style;
use ratatui::text::Span;

use crate::theme::{Theme, Token};

/// Lexical rules of one language.
#[derive(Debug)]
pub struct Lang {
    /// Canonical name.
    pub name: &'static str,
    /// Fence names and file extensions that select it.
    pub aliases: &'static [&'static str],
    /// Reserved words.
    pub keywords: &'static [&'static str],
    /// Line comment starters.
    pub line_comments: &'static [&'static str],
    /// Block comment delimiters.
    pub block_comment: Option<(&'static str, &'static str)>,
    /// String quote characters.
    pub quotes: &'static [char],
}

/// Lexical class of a piece of code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Plain code.
    Plain,
    /// Keyword.
    Keyword,
    /// String literal.
    String,
    /// Number literal.
    Number,
    /// Comment.
    Comment,
    /// Capitalised identifier (types, constants).
    Type,
}

impl Class {
    /// The theme token for this class.
    pub fn token(self) -> Token {
        match self {
            Class::Plain => Token::Fg,
            Class::Keyword => Token::Keyword,
            Class::String => Token::Number,
            Class::Number => Token::Accent,
            Class::Comment => Token::Muted,
            Class::Type => Token::Running,
        }
    }
}

const RUST_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const PY_KW: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while",
    "with", "yield",
];
const JS_KW: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "return",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
];
const GO_KW: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
    "true",
    "false",
];
const SH_KW: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in",
    "local", "return", "then", "until", "while",
];
const C_KW: &[&str] = &[
    "auto",
    "bool",
    "break",
    "case",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "float",
    "for",
    "if",
    "int",
    "long",
    "namespace",
    "new",
    "nullptr",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "true",
    "typedef",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "while",
];
const JAVA_KW: &[&str] = &[
    "abstract",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "for",
    "if",
    "implements",
    "import",
    "int",
    "interface",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "switch",
    "this",
    "throw",
    "throws",
    "true",
    "try",
    "void",
    "while",
];
const DATA_KW: &[&str] = &["true", "false", "null", "yes", "no"];

/// The language table, built on first use.
pub fn languages() -> &'static [Lang] {
    static LANGS: OnceLock<Vec<Lang>> = OnceLock::new();
    LANGS.get_or_init(|| {
        vec![
            Lang {
                name: "rust",
                aliases: &["rust", "rs"],
                keywords: RUST_KW,
                line_comments: &["//"],
                block_comment: Some(("/*", "*/")),
                quotes: &['"'],
            },
            Lang {
                name: "python",
                aliases: &["python", "py"],
                keywords: PY_KW,
                line_comments: &["#"],
                block_comment: None,
                quotes: &['"', '\''],
            },
            Lang {
                name: "javascript",
                aliases: &["javascript", "js", "jsx", "ts", "tsx", "typescript", "mjs"],
                keywords: JS_KW,
                line_comments: &["//"],
                block_comment: Some(("/*", "*/")),
                quotes: &['"', '\'', '`'],
            },
            Lang {
                name: "go",
                aliases: &["go", "golang"],
                keywords: GO_KW,
                line_comments: &["//"],
                block_comment: Some(("/*", "*/")),
                quotes: &['"', '`'],
            },
            Lang {
                name: "shell",
                aliases: &["sh", "bash", "shell", "zsh", "console"],
                keywords: SH_KW,
                line_comments: &["#"],
                block_comment: None,
                quotes: &['"', '\''],
            },
            Lang {
                name: "c",
                aliases: &["c", "h", "cpp", "cc", "hpp", "cxx", "c++"],
                keywords: C_KW,
                line_comments: &["//"],
                block_comment: Some(("/*", "*/")),
                quotes: &['"', '\''],
            },
            Lang {
                name: "java",
                aliases: &["java", "kt", "kotlin"],
                keywords: JAVA_KW,
                line_comments: &["//"],
                block_comment: Some(("/*", "*/")),
                quotes: &['"'],
            },
            Lang {
                name: "data",
                aliases: &["json", "toml", "yaml", "yml", "ini"],
                keywords: DATA_KW,
                line_comments: &["#", ";"],
                block_comment: None,
                quotes: &['"', '\''],
            },
        ]
    })
}

/// Finds a language by fence name (`rust`), extension (`rs`) or file path
/// (`src/main.rs`).
pub fn lang_for(name: &str) -> Option<&'static Lang> {
    let name = name.trim().to_ascii_lowercase();
    let key = name.rsplit_once('.').map_or(name.as_str(), |(_, ext)| ext);
    languages()
        .iter()
        .find(|l| l.aliases.contains(&key) || l.aliases.contains(&name.as_str()))
}

/// Highlighting state carried across the lines of one block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct State {
    in_block_comment: bool,
}

/// Splits one line into classified pieces. `state` carries open block
/// comments to the next line.
pub fn classify(lang: &Lang, line: &str, state: &mut State) -> Vec<(Class, String)> {
    let mut out: Vec<(Class, String)> = Vec::new();
    let push = |out: &mut Vec<(Class, String)>, class: Class, s: &str| {
        if s.is_empty() {
            return;
        }
        match out.last_mut() {
            Some((c, text)) if *c == class => text.push_str(s),
            _ => out.push((class, s.to_owned())),
        }
    };
    let mut rest = line;
    while !rest.is_empty() {
        if state.in_block_comment {
            let (_, end) = lang.block_comment.unwrap_or(("", ""));
            match rest.find(end).filter(|_| !end.is_empty()) {
                Some(i) => {
                    push(&mut out, Class::Comment, &rest[..i + end.len()]);
                    rest = &rest[i + end.len()..];
                    state.in_block_comment = false;
                }
                None => {
                    push(&mut out, Class::Comment, rest);
                    rest = "";
                }
            }
            continue;
        }
        if lang.line_comments.iter().any(|c| rest.starts_with(c)) {
            push(&mut out, Class::Comment, rest);
            break;
        }
        if let Some((start, _)) = lang.block_comment
            && rest.starts_with(start)
        {
            push(&mut out, Class::Comment, start);
            rest = &rest[start.len()..];
            state.in_block_comment = true;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        if lang.quotes.contains(&c) {
            let mut end = rest.len();
            let mut escaped = false;
            for (i, ch) in rest.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == c {
                    end = i + ch.len_utf8();
                    break;
                }
            }
            push(&mut out, Class::String, &rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if c.is_ascii_digit() {
            let end = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
                .unwrap_or(rest.len());
            push(&mut out, Class::Number, &rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let end = rest
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .unwrap_or(rest.len());
            let word = &rest[..end];
            let class = if lang.keywords.contains(&word) {
                Class::Keyword
            } else if word.chars().next().is_some_and(char::is_uppercase) {
                Class::Type
            } else {
                Class::Plain
            };
            push(&mut out, class, word);
            rest = &rest[end..];
            continue;
        }
        let len = c.len_utf8();
        push(&mut out, Class::Plain, &rest[..len]);
        rest = &rest[len..];
    }
    out
}

/// Highlights one line as styled spans. Without a language the line is
/// plain `fg`. `base` is patched under every span (e.g. a diff background).
pub fn highlight_line(
    lang: Option<&Lang>,
    line: &str,
    state: &mut State,
    theme: &Theme,
    base: Style,
) -> Vec<Span<'static>> {
    match lang {
        None => vec![Span::styled(
            line.to_owned(),
            base.patch(theme.fg(Token::Fg)),
        )],
        Some(lang) => classify(lang, line, state)
            .into_iter()
            .map(|(class, text)| Span::styled(text, base.patch(theme.fg(class.token()))))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_languages_by_name_extension_and_path() {
        assert_eq!(lang_for("rust").map(|l| l.name), Some("rust"));
        assert_eq!(lang_for("src/parse.rs").map(|l| l.name), Some("rust"));
        assert_eq!(lang_for("TS").map(|l| l.name), Some("javascript"));
        assert!(lang_for("brainfuck").is_none());
    }

    #[test]
    fn classifies_rust() {
        let lang = lang_for("rust").unwrap();
        let mut st = State::default();
        let parts = classify(lang, r#"let x: Foo = "a\"b"; // hi 42"#, &mut st);
        assert_eq!(parts[0], (Class::Keyword, "let".into()));
        assert!(parts.contains(&(Class::Type, "Foo".into())));
        assert!(parts.contains(&(Class::String, r#""a\"b""#.into())));
        assert_eq!(parts.last().unwrap(), &(Class::Comment, "// hi 42".into()));
        let parts = classify(lang, "x = 0x1f;", &mut st);
        assert!(parts.contains(&(Class::Number, "0x1f".into())));
    }

    #[test]
    fn block_comments_span_lines() {
        let lang = lang_for("c").unwrap();
        let mut st = State::default();
        let a = classify(lang, "int a; /* start", &mut st);
        assert_eq!(a.last().unwrap().0, Class::Comment);
        let b = classify(lang, "still */ int b;", &mut st);
        assert_eq!(b[0], (Class::Comment, "still */".into()));
        assert!(b.contains(&(Class::Keyword, "int".into())));
    }
}
