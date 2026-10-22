//! Unwrapping of commands that run other commands.

use super::{MAX_DEPTH, ParseError, SimpleCommand, Word, parse_at_depth};

/// Programs whose arguments contain another command that policy checks too.
pub const WRAPPERS: &[&str] = &[
    "env", "sudo", "doas", "command", "builtin", "exec", "nohup", "time", "nice", "ionice",
    "timeout", "stdbuf", "setsid", "unbuffer", "xargs", "watch", "bash", "sh", "zsh", "dash",
    "ksh", "eval", "find",
];

const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh"];

/// What a wrapper runs.
enum Inner {
    /// Nothing more (not a wrapper, or a wrapper without a command).
    None,
    /// Commands given as words.
    Commands(Vec<SimpleCommand>),
    /// A command given as a shell string (`bash -c`, `eval`, `watch`).
    Script(String),
}

/// Push `cmd` and, after it, every command it wraps.
pub(super) fn push_unwrapped(
    mut cmd: SimpleCommand,
    depth: usize,
    out: &mut Vec<SimpleCommand>,
) -> Result<(), ParseError> {
    if depth > MAX_DEPTH {
        return Err(ParseError::TooDeep);
    }
    if cmd.words.first().is_some_and(|w| w.dynamic) {
        return Err(ParseError::DynamicCommand(cmd.line()));
    }
    let inner = inner(&cmd)?;
    cmd.wrapper = !matches!(inner, Inner::None);
    out.push(cmd);
    match inner {
        Inner::None => {}
        Inner::Commands(cmds) => {
            for c in cmds {
                push_unwrapped(c, depth + 1, out)?;
            }
        }
        Inner::Script(src) => out.extend(parse_at_depth(&src, depth + 1)?.commands),
    }
    Ok(())
}

fn words_cmd(words: &[Word]) -> Inner {
    words_cmd_with(Vec::new(), words)
}

fn words_cmd_with(assignments: Vec<(String, Word)>, words: &[Word]) -> Inner {
    if words.is_empty() && assignments.is_empty() {
        return Inner::None;
    }
    Inner::Commands(vec![SimpleCommand {
        assignments,
        words: words.to_vec(),
        ..SimpleCommand::default()
    }])
}

/// Index of the first operand after options. `value_opts` take a separate
/// value. Stops at `--` (skipping it) or the first non-option.
fn skip_options(args: &[Word], value_opts: &[&str]) -> usize {
    let mut i = 0;
    while let Some(a) = args.get(i) {
        let t = a.text.as_str();
        if t == "--" {
            return i + 1;
        }
        if !t.starts_with('-') || t == "-" {
            return i;
        }
        i += if value_opts.contains(&t) { 2 } else { 1 };
    }
    args.len()
}

fn split_assignments(args: &[Word]) -> (Vec<(String, Word)>, &[Word]) {
    let mut assigns = Vec::new();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.text.split_once('=') {
            Some((name, value))
                if !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !name.starts_with(|c: char| c.is_ascii_digit()) =>
            {
                assigns.push((
                    name.to_string(),
                    Word {
                        text: value.to_string(),
                        dynamic: a.dynamic,
                    },
                ));
                i += 1;
            }
            _ => break,
        }
    }
    (assigns, &args[i..])
}

fn script_from(words: &[Word], what: &str) -> Result<Inner, ParseError> {
    if words.iter().any(|w| w.dynamic) {
        return Err(ParseError::DynamicCommand(format!(
            "{what} with run-time arguments"
        )));
    }
    let text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(if text.trim().is_empty() {
        Inner::None
    } else {
        Inner::Script(text)
    })
}

fn inner(cmd: &SimpleCommand) -> Result<Inner, ParseError> {
    let Some(name) = cmd.program_name() else {
        return Ok(Inner::None);
    };
    let args = cmd.args();
    let rest = |value_opts: &[&str]| &args[skip_options(args, value_opts).min(args.len())..];
    Ok(match name {
        "env" => {
            if args
                .iter()
                .any(|a| a.text == "-S" || a.text.starts_with("--split-string"))
            {
                return Err(ParseError::Unsupported("`env -S`".into()));
            }
            let (assigns, words) = split_assignments(rest(&["-u", "--unset", "-C", "--chdir"]));
            words_cmd_with(assigns, words)
        }
        "sudo" => {
            let r = rest(&[
                "-u",
                "-g",
                "-C",
                "-D",
                "-h",
                "-p",
                "-r",
                "-t",
                "-U",
                "-T",
                "--user",
                "--group",
                "--chdir",
                "--host",
                "--prompt",
                "--role",
                "--type",
                "--other-user",
                "--close-from",
                "--command-timeout",
            ]);
            let (assigns, words) = split_assignments(r);
            words_cmd_with(assigns, words)
        }
        "doas" => words_cmd(rest(&["-u", "-C"])),
        "command" => {
            if args.iter().any(|a| a.text == "-v" || a.text == "-V") {
                Inner::None
            } else {
                words_cmd(rest(&[]))
            }
        }
        "builtin" | "nohup" | "setsid" | "unbuffer" => words_cmd(rest(&[])),
        "exec" => words_cmd(rest(&["-a"])),
        "time" => words_cmd(rest(&["-f", "-o", "--format", "--output"])),
        "nice" => words_cmd(rest(&["-n", "--adjustment"])),
        "ionice" => words_cmd(rest(&[
            "-c",
            "-n",
            "-p",
            "-P",
            "-u",
            "--class",
            "--classdata",
        ])),
        "stdbuf" => words_cmd(rest(&["-i", "-o", "-e"])),
        "timeout" => {
            let r = rest(&["-s", "-k", "--signal", "--kill-after"]);
            words_cmd(r.get(1..).unwrap_or(&[]))
        }
        "xargs" => {
            let r = rest(&[
                "-a",
                "-d",
                "-E",
                "-I",
                "-L",
                "-n",
                "-P",
                "-s",
                "--arg-file",
                "--delimiter",
                "--eof",
                "--replace",
                "--max-lines",
                "--max-args",
                "--max-procs",
                "--max-chars",
                "--process-slot-var",
            ]);
            if r.is_empty() {
                words_cmd(&[Word::literal("echo")])
            } else {
                words_cmd(r)
            }
        }
        "watch" => script_from(rest(&["-n", "--interval", "-d", "--differences"]), "watch")?,
        "eval" => script_from(args, "eval")?,
        "find" => find_exec(args),
        n if SHELLS.contains(&n) => shell_c(args)?,
        _ => Inner::None,
    })
}

/// `find ... -exec cmd {} ;` and friends.
fn find_exec(args: &[Word]) -> Inner {
    let mut cmds = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if matches!(
            args[i].text.as_str(),
            "-exec" | "-execdir" | "-ok" | "-okdir"
        ) {
            let start = i + 1;
            let mut end = start;
            while end < args.len() && !matches!(args[end].text.as_str(), ";" | "+") {
                end += 1;
            }
            if end > start {
                cmds.push(SimpleCommand {
                    words: args[start..end].to_vec(),
                    ..SimpleCommand::default()
                });
            }
            i = end;
        }
        i += 1;
    }
    if cmds.is_empty() {
        Inner::None
    } else {
        Inner::Commands(cmds)
    }
}

/// `bash -c 'script'`, including combined flags such as `-lc` or `-ec`.
fn shell_c(args: &[Word]) -> Result<Inner, ParseError> {
    let mut i = 0;
    let mut has_c = false;
    while let Some(a) = args.get(i) {
        let t = a.text.as_str();
        if t == "--" {
            i += 1;
            break;
        }
        if matches!(t, "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file") {
            i += 2;
            continue;
        }
        if t.starts_with("--") {
            i += 1;
            continue;
        }
        if (t.starts_with('-') || t.starts_with('+')) && t.len() > 1 {
            if t[1..].contains('c') {
                has_c = true;
            }
            i += 1;
            continue;
        }
        break;
    }
    if !has_c {
        return Ok(Inner::None);
    }
    match args.get(i) {
        Some(script) => script_from(std::slice::from_ref(script), "shell -c"),
        None => Ok(Inner::None),
    }
}
