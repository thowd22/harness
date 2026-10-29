//! Git staging guard.
//!
//! A reflexive `git add -A` (or `git add .`, `git add --all`,
//! `git commit -a`) sweeps every change in the worktree into the commit,
//! including temp files, debug scripts and unrelated edits. The guard turns
//! those commands into an approval prompt (or a refusal, with
//! [`StagingGuard::deny`]) whose reason tells the model to stage explicit
//! paths instead. Staging explicit paths (`git add src/a.rs`) is not
//! affected; it is decided by the normal policy rules and mode.
//!
//! The guard only ever tightens a [`Verdict`]. It steps aside when the
//! deciding rule is a harness.ini `bash.allow` pattern or a session grant
//! whose fixed prefix is itself a sweeping command (`git add -A*`), i.e.
//! when the user explicitly allowed it.
//!
//! Use [`StagingGuard::decide`] in place of [`PolicyEngine::decide`]; it
//! records the final verdict to the given sink, so the engine itself
//! should keep its default no-op sink.

use std::path::Path;

use crate::rules::{
    Action, CommandPattern, Decision, DecisionRecord, DecisionSink, Layer, MatchedRule, Mode,
    PolicyEngine, Verdict,
};
use crate::shell::{self, SimpleCommand};

/// Origin of the guard's rule in decisions.
pub const STAGING_ORIGIN: &str = "built-in staging guard";

/// git global options that take a separate value.
const GIT_VALUE_OPTIONS: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
    "--config-env",
    "--super-prefix",
    "--list-cmds",
];

/// `git commit` short options whose value follows in the same cluster.
const COMMIT_VALUE_SHORTS: &[char] = &['m', 'F', 'C', 'c', 't', 'S', 'u'];

/// A command that stages everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sweep {
    /// `git add` with `-A`, `--all`, `.` or another whole-tree pathspec.
    Add {
        /// The argument that sweeps (`-A`, `.`, ...).
        arg: String,
    },
    /// `git commit` with `-a` / `--all`.
    Commit {
        /// The argument that sweeps (`-a`, `-am`, `--all`).
        arg: String,
    },
}

impl Sweep {
    /// The canonical form, e.g. `git add -A`.
    pub fn display(&self) -> String {
        match self {
            Self::Add { arg } => format!("git add {arg}"),
            Self::Commit { arg } => format!("git commit {arg}"),
        }
    }
}

/// Whole-tree pathspecs for `git add`.
fn is_whole_tree(arg: &str) -> bool {
    matches!(
        arg.trim_end_matches('/'),
        "." | "" | ":/" | ":" | "*" | ":/*" | ":(top)" | "./*" | ".*"
    ) || arg == "./"
}

/// If `cmd` is a sweeping `git add` / `git commit`, which kind.
pub fn sweeping(cmd: &SimpleCommand) -> Option<Sweep> {
    if cmd.program_name() != Some("git") {
        return None;
    }
    let args: Vec<&str> = cmd.args().iter().map(|w| w.text.as_str()).collect();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if !a.starts_with('-') || *a == "-" {
            break;
        }
        i += if GIT_VALUE_OPTIONS.contains(a) { 2 } else { 1 };
    }
    let sub = *args.get(i)?;
    let rest = args.get(i + 1..).unwrap_or_default();
    match sub {
        "add" | "stage" => sweeping_add(rest),
        "commit" => sweeping_commit(rest),
        _ => None,
    }
}

fn sweeping_add(args: &[&str]) -> Option<Sweep> {
    let mut paths_only = false;
    for a in args {
        if !paths_only {
            if *a == "--" {
                paths_only = true;
                continue;
            }
            if *a == "--all" || *a == "--no-ignore-removal" || *a == "-A" {
                return Some(Sweep::Add { arg: a.to_string() });
            }
            if let Some(cluster) = a.strip_prefix('-')
                && !cluster.starts_with('-')
            {
                if cluster.contains('A') {
                    return Some(Sweep::Add { arg: a.to_string() });
                }
                continue;
            }
            if a.starts_with("--") {
                continue;
            }
        }
        if is_whole_tree(a) {
            return Some(Sweep::Add { arg: a.to_string() });
        }
    }
    None
}

fn sweeping_commit(args: &[&str]) -> Option<Sweep> {
    let mut skip_value = false;
    for a in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        if *a == "--" {
            break;
        }
        if *a == "--all" {
            return Some(Sweep::Commit { arg: a.to_string() });
        }
        if matches!(
            *a,
            "--message"
                | "--file"
                | "--reuse-message"
                | "--reedit-message"
                | "--template"
                | "--author"
                | "--date"
                | "--fixup"
                | "--squash"
        ) {
            skip_value = true;
            continue;
        }
        let Some(cluster) = a.strip_prefix('-').filter(|c| !c.starts_with('-')) else {
            continue;
        };
        for (idx, ch) in cluster.char_indices() {
            if ch == 'a' {
                return Some(Sweep::Commit { arg: a.to_string() });
            }
            if COMMIT_VALUE_SHORTS.contains(&ch) {
                // The value is the rest of the cluster or the next word.
                skip_value = idx + ch.len_utf8() == cluster.len();
                break;
            }
        }
    }
    None
}

/// The first sweeping staging command in a bash command line, with the
/// simple command it was found in.
pub fn check(command: &str) -> Option<(String, Sweep)> {
    let script = shell::parse(command).ok()?;
    script
        .commands
        .iter()
        .find_map(|c| sweeping(c).map(|s| (c.line(), s)))
}

/// The guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagingGuard {
    decision: Decision,
}

impl Default for StagingGuard {
    /// Ask before sweeping staging.
    fn default() -> Self {
        Self {
            decision: Decision::Ask,
        }
    }
}

impl StagingGuard {
    /// Ask before sweeping staging (the default).
    pub fn ask() -> Self {
        Self::default()
    }

    /// Refuse sweeping staging.
    pub fn deny() -> Self {
        Self {
            decision: Decision::Deny,
        }
    }

    /// The decision the guard enforces.
    pub fn decision(&self) -> Decision {
        self.decision
    }

    /// The guard's rule for a sweeping `line`.
    pub fn rule(&self, line: &str, sweep: &Sweep) -> MatchedRule {
        MatchedRule {
            layer: Layer::BuiltIn,
            origin: STAGING_ORIGIN.into(),
            rule: sweep.display(),
            reason: format!(
                "`{line}` stages every change in the worktree, which sweeps in temp files and \
                 unrelated edits. Stage explicit paths instead, e.g. `git add src/parser.rs \
                 tests/parser.rs`, then run `git commit` without `-a`."
            ),
        }
    }

    /// Tighten `verdict` for `action` if it is a sweeping staging command.
    pub fn apply(&self, action: &Action, verdict: Verdict) -> Verdict {
        let Action::Bash { command } = action else {
            return verdict;
        };
        let Some((line, sweep)) = check(command) else {
            return verdict;
        };
        if verdict.decision >= self.decision || explicitly_allowed(&verdict) {
            return verdict;
        }
        Verdict {
            decision: self.decision,
            rule: self.rule(&line, &sweep),
            command: Some(line),
            outward: verdict.outward,
        }
    }

    /// Decide `action` with `engine` (without recording), apply the
    /// guard, and record the final verdict to `sink`.
    pub fn decide(
        &self,
        engine: &PolicyEngine,
        action: &Action,
        mode: Mode,
        cwd: &Path,
        sink: &dyn DecisionSink,
    ) -> Verdict {
        let verdict = self
            .apply(action, engine.check(action, mode, cwd))
            .in_mode(mode);
        sink.record(&DecisionRecord {
            tool: action.tool().to_string(),
            action: action.summary(),
            mode,
            verdict: verdict.clone(),
        });
        verdict
    }
}

/// True when the user explicitly allowed a sweeping command: the deciding
/// rule is an ini allow or session grant whose fixed prefix sweeps.
fn explicitly_allowed(v: &Verdict) -> bool {
    v.decision == Decision::Allow
        && matches!(v.rule.layer, Layer::Ini | Layer::Session)
        && check(
            CommandPattern::new(&v.rule.rule)
                .literal_prefix()
                .trim_end(),
        )
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{PolicyConfig, RecordingDecisions};

    fn sweeps(cmd: &str) -> bool {
        check(cmd).is_some()
    }

    #[test]
    fn detects_sweeping_staging() {
        for c in [
            "git add -A",
            "git add --all",
            "git add .",
            "git add ./",
            "git add :/",
            "git add -v -A",
            "git add -fA",
            "git -C repo add .",
            "git --no-pager add --all",
            "/usr/bin/git add -A",
            "git add -- .",
            "git stage .",
            "git commit -a",
            "git commit -am 'fix bug'",
            "git commit --all -m x",
            "git commit -v -a",
            "cargo fmt && git add -A && git commit -m wip",
            "sudo git add .",
            "bash -c 'git add -A'",
        ] {
            assert!(sweeps(c), "{c}");
        }
    }

    #[test]
    fn explicit_paths_are_not_sweeping() {
        for c in [
            "git add src/lib.rs tests/lib.rs",
            "git add -p src/lib.rs",
            "git add -u",
            "git add ./src/lib.rs",
            "git add .gitignore",
            "git commit -m 'add a feature'",
            "git commit -m all",
            "git commit -ma",
            "git commit --message -a",
            "git commit -- a.txt",
            "git status .",
            "echo git add -A",
            "git log --all",
        ] {
            assert!(!sweeps(c), "{c}");
        }
    }

    fn engine(allow: &[&str]) -> PolicyEngine {
        let cfg = PolicyConfig {
            bash_allow: allow.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        PolicyEngine::new(&cfg, "/w").unwrap()
    }

    fn bash(c: &str) -> Action {
        Action::Bash { command: c.into() }
    }

    #[test]
    fn yolo_sweeps_staging_without_asking_and_deny_still_refuses() {
        let e = engine(&[]);
        let sink = RecordingDecisions::default();
        let v = StagingGuard::default().decide(
            &e,
            &bash("git add -A"),
            Mode::Yolo,
            Path::new("/w"),
            &sink,
        );
        assert_eq!(v.decision, Decision::Allow);
        assert!(v.rule.reason.contains(STAGING_ORIGIN), "{}", v.rule.reason);
        assert_eq!(sink.records()[0].verdict, v);
        let v = StagingGuard::deny().decide(
            &e,
            &bash("git add -A"),
            Mode::Yolo,
            Path::new("/w"),
            &sink,
        );
        assert_eq!(v.decision, Decision::Deny);
    }

    #[test]
    fn asks_by_default_with_a_message_to_stage_explicit_paths() {
        let e = engine(&[]);
        let sink = RecordingDecisions::default();
        let g = StagingGuard::default();
        let v = g.decide(&e, &bash("git add -A"), Mode::Auto, Path::new("/w"), &sink);
        assert_eq!(v.decision, Decision::Ask);
        assert_eq!(v.rule.origin, STAGING_ORIGIN);
        assert_eq!(v.rule.rule, "git add -A");
        assert!(
            v.rule.reason.contains("Stage explicit paths"),
            "{}",
            v.rule.reason
        );
        let recs = sink.records();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].verdict.rule.origin, STAGING_ORIGIN);

        let v = StagingGuard::deny().decide(
            &e,
            &bash("git commit -am x"),
            Mode::Auto,
            Path::new("/w"),
            &sink,
        );
        assert_eq!(v.decision, Decision::Deny);
        assert_eq!(v.command.as_deref(), Some("git commit -am x"));
    }

    #[test]
    fn explicit_path_staging_follows_policy() {
        let g = StagingGuard::default();
        let sink = RecordingDecisions::default();
        let cwd = Path::new("/w");
        // Auto mode allows it; the guard does not interfere.
        let v = g.decide(
            &engine(&[]),
            &bash("git add src/a.rs"),
            Mode::Auto,
            cwd,
            &sink,
        );
        assert_eq!(v.decision, Decision::Allow);
        // An ini allow rule for explicit adds allows it in auto-edit mode.
        let v = g.decide(
            &engine(&["git add *"]),
            &bash("git add src/a.rs"),
            Mode::AutoEdit,
            cwd,
            &sink,
        );
        assert_eq!(v.decision, Decision::Allow);
        // ...but a broad allow rule does not let `git add -A` through.
        let v = g.decide(
            &engine(&["git add *"]),
            &bash("git add -A"),
            Mode::AutoEdit,
            cwd,
            &sink,
        );
        assert_eq!(v.decision, Decision::Ask);
        // An allow rule that names the sweep explicitly does.
        let v = g.decide(
            &engine(&["git add -A*"]),
            &bash("git add -A"),
            Mode::AutoEdit,
            cwd,
            &sink,
        );
        assert_eq!(v.decision, Decision::Allow);
    }

    #[test]
    fn never_loosens() {
        let cfg = PolicyConfig {
            bash_deny: vec!["git add*".into()],
            ..Default::default()
        };
        let e = PolicyEngine::new(&cfg, "/w").unwrap();
        let v = StagingGuard::default().decide(
            &e,
            &bash("git add -A"),
            Mode::Auto,
            Path::new("/w"),
            &RecordingDecisions::default(),
        );
        assert_eq!(v.decision, Decision::Deny);
        assert_ne!(v.rule.origin, STAGING_ORIGIN);
        let read = Action::Read {
            tool: "read".into(),
            path: "a".into(),
        };
        let base = e.check(&read, Mode::Auto, Path::new("/w"));
        assert_eq!(StagingGuard::default().apply(&read, base.clone()), base);
    }
}
