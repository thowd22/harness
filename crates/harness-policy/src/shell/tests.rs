use super::*;

fn lines(src: &str) -> Vec<String> {
    parse(src)
        .unwrap_or_else(|e| panic!("{src:?}: {e}"))
        .commands
        .iter()
        .map(SimpleCommand::line)
        .collect()
}

fn err(src: &str) -> ParseError {
    parse(src).expect_err(src)
}

#[test]
fn simple_command_and_quotes() {
    assert_eq!(lines("ls -la"), ["ls -la"]);
    assert_eq!(lines("echo 'a b' \"c d\" e\\ f"), ["echo a b c d e f"]);
    assert_eq!(lines("g''it pu\"\"sh"), ["git push"]);
    assert_eq!(lines("\\git push"), ["git push"]);
    assert_eq!(lines("$'\\x67it' push"), ["git push"]);
    assert_eq!(lines("$'\\147it' push"), ["git push"]);
    assert_eq!(lines("$\"git\" push"), ["git push"]);
}

#[test]
fn lists_and_pipelines_are_split() {
    assert_eq!(
        lines("cargo build && git push || echo no; ls & rm x | wc -l |& cat"),
        [
            "cargo build",
            "git push",
            "echo no",
            "ls",
            "rm x",
            "wc -l",
            "cat"
        ]
    );
    assert_eq!(lines("ls\ngit push\n"), ["ls", "git push"]);
    assert_eq!(lines("ls # git push\nrm a"), ["ls", "rm a"]);
}

#[test]
fn subshells_groups_and_control_flow() {
    assert_eq!(lines("(cd x && git push)"), ["cd x", "git push"]);
    assert_eq!(lines("{ ls; git push; }"), ["ls", "git push"]);
    assert_eq!(
        lines("if test -f a; then git push; elif x; then y; else rm b; fi"),
        ["test -f a", "git push", "x", "y", "rm b"]
    );
    assert_eq!(lines("while true; do git push; done"), ["true", "git push"]);
    assert_eq!(lines("for f in a $(ls) c; do rm $f; done"), ["ls", "rm $f"]);
    assert_eq!(lines("! git push"), ["git push"]);
    assert_eq!(
        lines("[[ -f a && -d b ]] && git push"),
        ["[[ -f a && -d b ]]", "git push"]
    );
}

#[test]
fn substitutions_are_checked() {
    assert_eq!(lines("echo $(git push)"), ["git push", "echo $(...)"]);
    assert_eq!(lines("echo \"$(git push)\""), ["git push", "echo $(...)"]);
    assert_eq!(lines("echo `git push`"), ["git push", "echo `...`"]);
    assert_eq!(
        lines("echo $(echo $(git push))"),
        ["git push", "echo $(...)", "echo $(...)"]
    );
    assert_eq!(
        lines("diff <(git push) >(rm x)"),
        ["git push", "rm x", "diff <(...) >(...)"]
    );
    assert_eq!(lines("echo $((1 + 2))"), ["echo $((1 + 2))"]);
}

#[test]
fn env_prefixes_and_redirects() {
    let s = parse("FOO=1 BAR=\"x y\" git push 2>&1 > out.txt >> log <in").unwrap();
    let c = &s.commands[0];
    assert_eq!(c.line(), "git push");
    assert_eq!(c.assignments[0].0, "FOO");
    assert_eq!(c.assignments[1].1.text, "x y");
    let ops: Vec<_> = c
        .redirects
        .iter()
        .map(|r| (r.fd, r.op, r.target.text.as_str()))
        .collect();
    assert_eq!(
        ops,
        [
            (Some(2), RedirectOp::Duplicate, "1"),
            (None, RedirectOp::Output, "out.txt"),
            (None, RedirectOp::Append, "log"),
            (None, RedirectOp::Input, "in"),
        ]
    );
    assert_eq!(
        c.path_candidates(),
        ["push", "1", "x y", "out.txt", "log", "in"]
    );
    let bare = parse("> ~/.ssh/authorized_keys").unwrap();
    assert!(bare.commands[0].words.is_empty());
    assert_eq!(
        bare.commands[0].path_candidates(),
        ["~/.ssh/authorized_keys"]
    );
    assert_eq!(
        parse("ls &> all").unwrap().commands[0].redirects[0].op,
        RedirectOp::OutputAll
    );
    assert_eq!(
        parse("ls >& all").unwrap().commands[0].redirects[0].op,
        RedirectOp::OutputAll
    );
}

#[test]
fn path_candidates_include_args_values_and_targets() {
    let s = parse("X=~/.aws/creds tool --file=~/.ssh/id_rsa a/b > out").unwrap();
    assert_eq!(
        s.commands[0].path_candidates(),
        [
            "--file=~/.ssh/id_rsa",
            "~/.ssh/id_rsa",
            "a/b",
            "~/.aws/creds",
            "out"
        ]
    );
}

#[test]
fn heredocs_are_skipped() {
    assert_eq!(lines("cat <<EOF > f\ngit push\nEOF\nls"), ["cat", "ls"]);
    assert_eq!(lines("cat <<-'X'\n\t$(git push)\n\tX\n"), ["cat"]);
    assert!(matches!(
        err("cat <<EOF\n$(git push)\nEOF"),
        ParseError::Unsupported(_)
    ));
    assert!(matches!(
        err("cat <<EOF\nno end"),
        ParseError::Unterminated(_)
    ));
}

#[test]
fn brace_expansion() {
    assert_eq!(lines("gi{t,} push"), ["git gi push"]);
    assert_eq!(lines("git {push,status}"), ["git push status"]);
    assert_eq!(lines("echo {p..p}ush"), ["echo push"]);
    assert_eq!(lines("echo {1..3}"), ["echo 1 2 3"]);
    assert_eq!(lines("echo '{a,b}' {} {x}"), ["echo {a,b} {} {x}"]);
    assert!(parse("echo {1..100000}").is_err());
}

#[test]
fn wrappers_are_unwrapped() {
    let cases: &[(&str, &[&str])] = &[
        (
            "sudo -u root git push",
            &["sudo -u root git push", "git push"],
        ),
        ("env -i A=1 git push", &["env -i A=1 git push", "git push"]),
        ("command git push", &["command git push", "git push"]),
        (
            "nohup nice -n 5 git push",
            &["nohup nice -n 5 git push", "nice -n 5 git push", "git push"],
        ),
        (
            "timeout -s KILL 10 git push",
            &["timeout -s KILL 10 git push", "git push"],
        ),
        ("xargs -n1 git push", &["xargs -n1 git push", "git push"]),
        (
            "bash -c 'cd x && git push'",
            &["bash -c cd x && git push", "cd x", "git push"],
        ),
        ("sh -lc \"git push\"", &["sh -lc git push", "git push"]),
        ("eval git push", &["eval git push", "git push"]),
        (
            "find . -exec git push {} \\;",
            &["find . -exec git push {} ;", "git push {}"],
        ),
        ("exec git push", &["exec git push", "git push"]),
        ("time git push", &["time git push", "git push"]),
    ];
    for (src, want) in cases {
        assert_eq!(&lines(src), want, "{src}");
    }
    let s = parse("sudo git push").unwrap();
    assert!(s.commands[0].wrapper);
    assert!(!s.commands[1].wrapper);
    assert!(!parse("bash script.sh").unwrap().commands[0].wrapper);
    assert!(!parse("command -v git").unwrap().commands[0].wrapper);
}

#[test]
fn program_forms() {
    let s = parse("/usr/bin/git push").unwrap();
    assert_eq!(s.commands[0].program_name(), Some("git"));
    assert_eq!(s.commands[0].forms(), ["/usr/bin/git push", "git push"]);
}

#[test]
fn dynamic_words_are_flagged() {
    let s = parse("rm -rf $DIR/x ${HOME}").unwrap();
    assert!(s.commands[0].is_dynamic());
    assert_eq!(s.commands[0].line(), "rm -rf $DIR/x ${HOME}");
    assert!(!parse("rm -rf '$DIR'").unwrap().commands[0].is_dynamic());
}

#[test]
fn unparseable_shapes_are_errors() {
    assert!(matches!(err("$CMD push"), ParseError::DynamicCommand(_)));
    assert!(matches!(
        err("$(echo git) push"),
        ParseError::DynamicCommand(_)
    ));
    assert!(matches!(
        err("bash -c \"$X\""),
        ParseError::DynamicCommand(_)
    ));
    assert!(matches!(err("eval \"$X\""), ParseError::DynamicCommand(_)));
    assert!(matches!(
        err("echo 'unterminated"),
        ParseError::Unterminated(_)
    ));
    assert!(matches!(
        err("echo \"unterminated"),
        ParseError::Unterminated(_)
    ));
    assert!(matches!(err("echo $(ls"), ParseError::Unterminated(_)));
    assert!(matches!(err("echo `ls"), ParseError::Unterminated(_)));
    assert!(matches!(err("ls )"), ParseError::UnbalancedParen));
    assert!(matches!(
        err("f() { git push; }; f"),
        ParseError::Unsupported(_)
    ));
    assert!(matches!(
        err("function f { git push; }"),
        ParseError::Unsupported(_)
    ));
    assert!(matches!(
        err("case x in a) git push;; esac"),
        ParseError::Unsupported(_)
    ));
    assert!(matches!(err("(( x++ ))"), ParseError::Unsupported(_)));
    assert!(matches!(
        err("for ((i=0;i<3;i++)); do ls; done"),
        ParseError::Unsupported(_)
    ));
    assert!(matches!(
        err("env -S 'git push'"),
        ParseError::Unsupported(_)
    ));
    assert!(matches!(err("a=(1 2)"), ParseError::Unsupported(_)));
    let deep = "( ".repeat(40) + &")".repeat(40);
    assert!(matches!(err(&deep), ParseError::TooDeep));
    let deep_eval = "eval ".repeat(40) + "ls";
    assert!(matches!(err(&deep_eval), ParseError::TooDeep));
}

#[test]
fn empty_input() {
    assert!(parse("").unwrap().commands.is_empty());
    assert!(parse("  ;; ").is_err());
    assert!(parse("  ; \n ").unwrap().commands.is_empty());
}
