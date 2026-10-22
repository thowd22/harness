//! Command-line parsing for the `harness` binary (hand-rolled: the surface is
//! small and keeps the binary free of a CLI framework dependency).

use std::path::PathBuf;

/// Exit codes of `harness run`.
pub mod exit {
    /// The agent finished (and verification passed or was not configured).
    pub const SUCCESS: u8 = 0;
    /// Unrecoverable error (provider failure, bad config, ...).
    pub const ERROR: u8 = 1;
    /// Invalid command line.
    pub const USAGE: u8 = 2;
    /// The agent finished but verification failed.
    pub const VERIFY_FAILED: u8 = 3;
    /// A token or cost budget was exhausted.
    pub const BUDGET: u8 = 4;
    /// The `max_turns` limit was reached.
    pub const MAX_TURNS: u8 = 5;
    /// Cancelled (Ctrl+C / SIGINT / SIGTERM).
    pub const CANCELLED: u8 = 130;
}

/// What headless mode does with calls that need approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Approvals {
    /// Deny every call that would ask for approval.
    #[default]
    Deny,
    /// Allow only calls matching an `--allow` pattern (or an ini
    /// `bash.allow` rule); deny the rest.
    Allowlist,
}

/// Options of `harness run`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunArgs {
    /// The prompt (all positional words joined by spaces; `-` reads stdin).
    pub prompt: String,
    /// Stream redacted events as JSON lines on stdout.
    pub json: bool,
    /// Agent profile name.
    pub agent: Option<String>,
    /// Model alias or `provider/model`.
    pub model: Option<String>,
    /// Approval behaviour.
    pub approvals: Approvals,
    /// Command patterns allowed in allowlist mode.
    pub allow: Vec<String>,
    /// Autonomy mode override (`ask`, `auto-edit`, `auto`).
    pub mode: Option<String>,
    /// Turn limit override.
    pub max_turns: Option<u32>,
    /// Cost limit for this run in USD.
    pub budget_usd: Option<f64>,
    /// Token limit for this run.
    pub budget_tokens: Option<u64>,
    /// Skip the `[verify]` command.
    pub no_verify: bool,
    /// Session to resume (id, id prefix or `last`).
    pub resume: Option<String>,
    /// Scripted fake provider (tests, demos).
    pub fake_script: Option<PathBuf>,
    /// Working directory (defaults to the current directory).
    pub cwd: Option<PathBuf>,
}

/// Usage text of `harness run`.
pub const RUN_USAGE: &str = "\
usage: harness run [OPTIONS] <PROMPT>...

Runs an agent headlessly in the current directory.

Options:
  --json                  stream redacted events as JSON lines on stdout
  --agent NAME            agent profile (default: coder)
  --model REF             model alias or provider/model
  --approvals deny|allowlist
                          calls that need approval: deny them all (default),
                          or allow only those matching --allow patterns
  --allow PATTERN         command pattern allowed in allowlist mode (repeatable)
  --mode ask|auto-edit|auto
                          autonomy mode for this run
  --max-turns N           stop after N model turns
  --budget-usd X          stop when this run has cost X dollars
  --budget-tokens N       stop when this run has used N tokens
  --no-verify             do not run the [verify] command
  --resume ID|last        continue a saved session
  --cwd DIR               project directory (default: current directory)
  --fake-script FILE      use the scripted fake provider (testing)

Exit codes: 0 success, 1 error, 2 usage, 3 verification failed,
            4 budget exhausted, 5 max turns reached, 130 cancelled";

fn value<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<&'a String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn number<T: std::str::FromStr>(s: &str, flag: &str) -> Result<T, String> {
    s.parse()
        .map_err(|_| format!("{flag}: `{s}` is not a valid number"))
}

/// Parses the arguments after `run`.
pub fn parse_run(args: &[String]) -> Result<RunArgs, String> {
    let mut out = RunArgs::default();
    let mut words = Vec::new();
    let mut it = args.iter();
    let mut approvals_set = false;
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        let take = |it: &mut std::slice::Iter<'_, String>| -> Result<String, String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => value(it, flag).cloned(),
            }
        };
        match flag {
            "--json" => out.json = true,
            "--no-verify" => out.no_verify = true,
            "--agent" => out.agent = Some(take(&mut it)?),
            "--model" => out.model = Some(take(&mut it)?),
            "--mode" => out.mode = Some(take(&mut it)?),
            "--resume" => out.resume = Some(take(&mut it)?),
            "--cwd" => out.cwd = Some(PathBuf::from(take(&mut it)?)),
            "--fake-script" => out.fake_script = Some(PathBuf::from(take(&mut it)?)),
            "--allow" => out.allow.push(take(&mut it)?),
            "--max-turns" => out.max_turns = Some(number(&take(&mut it)?, flag)?),
            "--budget-usd" => out.budget_usd = Some(number(&take(&mut it)?, flag)?),
            "--budget-tokens" => out.budget_tokens = Some(number(&take(&mut it)?, flag)?),
            "--approvals" => {
                approvals_set = true;
                out.approvals = match take(&mut it)?.as_str() {
                    "deny" => Approvals::Deny,
                    "allowlist" | "allow-listed" => Approvals::Allowlist,
                    other => {
                        return Err(format!(
                            "--approvals: expected deny or allowlist, got `{other}`"
                        ));
                    }
                }
            }
            "--" => words.extend(it.by_ref().cloned()),
            f if f.starts_with("--") => return Err(format!("unknown option `{f}`")),
            _ => words.push(arg.clone()),
        }
    }
    if !out.allow.is_empty() && out.approvals != Approvals::Allowlist {
        return Err(if approvals_set {
            "--allow only applies with --approvals allowlist".into()
        } else {
            "--allow needs --approvals allowlist".into()
        });
    }
    out.prompt = words.join(" ");
    if out.prompt.trim().is_empty() {
        return Err("a prompt is required".into());
    }
    Ok(out)
}

/// Options of `harness wf run`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WfArgs {
    /// Workflow name or slash command (`fix-bug` or `fix`).
    pub name: String,
    /// `--input name=value` pairs, in order.
    pub inputs: Vec<(String, String)>,
    /// Remaining words, parsed like the workflow's slash command arguments.
    pub args: Vec<String>,
    /// Stream redacted events as JSON lines on stdout.
    pub json: bool,
    /// Approval behaviour for tool calls of agent steps.
    pub approvals: Approvals,
    /// Command patterns allowed in allowlist mode.
    pub allow: Vec<String>,
    /// Autonomy mode for agent steps.
    pub mode: Option<String>,
    /// Accepted for symmetry with `harness run`; steps name their agents.
    pub agent: Option<String>,
    /// Model for agent steps that do not name one.
    pub model: Option<String>,
    /// Turn limit for each agent step.
    pub max_turns: Option<u32>,
    /// Cost limit for the whole workflow run in USD.
    pub budget_usd: Option<f64>,
    /// Approve every approval gate.
    pub approve_all: bool,
    /// Approve the gates of these steps.
    pub approve: Vec<String>,
    /// Deny the gates of these steps (wins over approvals).
    pub deny: Vec<String>,
    /// Resume this earlier run (id, prefix or `last`).
    pub resume: Option<String>,
    /// Re-run this earlier run.
    pub rerun: Option<String>,
    /// With `--rerun`: steps to re-run (default: the failed ones).
    pub from: Vec<String>,
    /// Project directory.
    pub cwd: Option<PathBuf>,
    /// Scripted fake provider for every agent step (testing).
    pub fake_script: Option<PathBuf>,
}

/// Usage text of `harness wf`.
pub const WF_USAGE: &str = "\
usage: harness wf list
       harness wf help <NAME>
       harness wf run <NAME> [OPTIONS] [ARGS]...

Runs a workflow headlessly. NAME is the workflow name or its slash command.
ARGS are parsed like the slash command's arguments (see `harness wf help`).

Options:
  --input NAME=VALUE      set an input (repeatable)
  --json                  stream redacted events as JSON lines on stdout
  --approvals deny|allowlist, --allow PATTERN
                          tool calls of agent steps that need approval
  --mode ask|auto-edit|auto
                          autonomy mode for agent steps
  --model REF             model for agent steps that do not name one
  --max-turns N           turn limit for each agent step
  --budget-usd X          cost limit for the workflow run
  --approve-all           approve every approval gate
  --approve STEP          approve the gate of STEP (repeatable)
  --deny STEP             deny the gate of STEP (repeatable; wins)
  --resume RUN            resume an earlier run (id, prefix or last)
  --rerun RUN [--from STEP]...
                          re-run an earlier run from STEP (default: failed steps)
  --cwd DIR               project directory (default: current directory)
  --fake-script FILE      use the scripted fake provider (testing)

Exit codes: 0 succeeded, 1 failed, 2 usage, 130 cancelled";

/// Parses the arguments after `wf run`.
pub fn parse_wf(args: &[String]) -> Result<WfArgs, String> {
    let mut out = WfArgs::default();
    let mut it = args.iter();
    let mut words = Vec::new();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        let take = |it: &mut std::slice::Iter<'_, String>| -> Result<String, String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => value(it, flag).cloned(),
            }
        };
        match flag {
            "--json" => out.json = true,
            "--approve-all" => out.approve_all = true,
            "--input" => {
                let v = take(&mut it)?;
                let (k, v) = v
                    .split_once('=')
                    .ok_or_else(|| format!("--input: expected NAME=VALUE, got `{v}`"))?;
                out.inputs.push((k.to_owned(), v.to_owned()));
            }
            "--approve" => out.approve.push(take(&mut it)?),
            "--deny" => out.deny.push(take(&mut it)?),
            "--from" => out.from.push(take(&mut it)?),
            "--resume" => out.resume = Some(take(&mut it)?),
            "--rerun" => out.rerun = Some(take(&mut it)?),
            "--agent" => out.agent = Some(take(&mut it)?),
            "--model" => out.model = Some(take(&mut it)?),
            "--mode" => out.mode = Some(take(&mut it)?),
            "--cwd" => out.cwd = Some(PathBuf::from(take(&mut it)?)),
            "--fake-script" => out.fake_script = Some(PathBuf::from(take(&mut it)?)),
            "--allow" => out.allow.push(take(&mut it)?),
            "--max-turns" => out.max_turns = Some(number(&take(&mut it)?, flag)?),
            "--budget-usd" => out.budget_usd = Some(number(&take(&mut it)?, flag)?),
            "--approvals" => {
                out.approvals = match take(&mut it)?.as_str() {
                    "deny" => Approvals::Deny,
                    "allowlist" | "allow-listed" => Approvals::Allowlist,
                    other => {
                        return Err(format!(
                            "--approvals: expected deny or allowlist, got `{other}`"
                        ));
                    }
                }
            }
            "--" => words.extend(it.by_ref().cloned()),
            _ => words.push(arg.clone()),
        }
    }
    if !out.allow.is_empty() && out.approvals != Approvals::Allowlist {
        return Err("--allow needs --approvals allowlist".into());
    }
    if out.resume.is_some() && out.rerun.is_some() {
        return Err("--resume and --rerun cannot be combined".into());
    }
    if !out.from.is_empty() && out.rerun.is_none() {
        return Err("--from needs --rerun".into());
    }
    let mut words = words.into_iter();
    out.name = words
        .next()
        .ok_or_else(|| "a workflow name is required".to_owned())?;
    out.args = words.collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_flags_and_prompt() {
        let a = parse_run(&args(
            "--json --agent rev --model=sonnet --max-turns 3 --budget-usd 0.5 --approvals allowlist --allow cargo* fix the bug",
        ))
        .unwrap();
        assert!(a.json);
        assert_eq!(a.agent.as_deref(), Some("rev"));
        assert_eq!(a.model.as_deref(), Some("sonnet"));
        assert_eq!(a.max_turns, Some(3));
        assert_eq!(a.budget_usd, Some(0.5));
        assert_eq!(a.approvals, Approvals::Allowlist);
        assert_eq!(a.allow, ["cargo*"]);
        assert_eq!(a.prompt, "fix the bug");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_run(&args("")).unwrap_err().contains("prompt"));
        assert!(
            parse_run(&args("--bogus x"))
                .unwrap_err()
                .contains("unknown option")
        );
        assert!(
            parse_run(&args("--max-turns lots x"))
                .unwrap_err()
                .contains("not a valid number")
        );
        assert!(
            parse_run(&args("--allow ls x"))
                .unwrap_err()
                .contains("allowlist")
        );
        assert!(
            parse_run(&args("--approvals maybe x"))
                .unwrap_err()
                .contains("deny or allowlist")
        );
        assert!(
            parse_run(&args("--agent"))
                .unwrap_err()
                .contains("needs a value")
        );
    }

    #[test]
    fn parses_workflow_runs() {
        let a = parse_wf(&args(
            "fix-bug --input issue=crash --json --approve plan --deny merge --mode auto --budget-usd 2 extra words",
        ))
        .unwrap();
        assert_eq!(a.name, "fix-bug");
        assert_eq!(a.inputs, [("issue".to_owned(), "crash".to_owned())]);
        assert_eq!(a.args, ["extra", "words"]);
        assert!(a.json);
        assert_eq!(a.approve, ["plan"]);
        assert_eq!(a.deny, ["merge"]);
        assert_eq!(a.budget_usd, Some(2.0));
        // Unknown --flags are slash-command arguments (--test cmd).
        let a = parse_wf(&args("ivr task --test cargo")).unwrap();
        assert_eq!(a.args, ["task", "--test", "cargo"]);
        assert!(parse_wf(&args("")).unwrap_err().contains("name"));
        assert!(
            parse_wf(&args("x --input nope"))
                .unwrap_err()
                .contains("NAME=VALUE")
        );
        assert!(
            parse_wf(&args("x --from a"))
                .unwrap_err()
                .contains("--rerun")
        );
        assert!(
            parse_wf(&args("x --resume a --rerun b"))
                .unwrap_err()
                .contains("cannot be combined")
        );
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(
            parse_run(&args("-- --json is text")).unwrap().prompt,
            "--json is text"
        );
    }
}
