//! `harness eval`: command-line entry points for the binary.
//!
//! ```text
//! harness eval run SUITE [--out FILE] [--parallel N] [--filter ID] [--repeats N]
//!                        [--model REF] [--keep-failed] [--in-process] [--shared-data-dir]
//!                        [--baseline FILE] [--max-pass-drop PTS] [--max-cost-rise PCT]
//! harness eval compare BASELINE CURRENT [--json] [--max-pass-drop PTS] [--max-cost-rise PCT]
//! ```
//!
//! The binary calls [`main`] with the arguments after `eval` and a
//! [`Context`] (its own path, version and the config hash).

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::baseline::{Thresholds, compare};
use crate::runner::exec::{Executor, InProcessExecutor, ProcessExecutor};
use crate::runner::{Results, RunOptions, Stamp, run_suite};
use crate::suite::{ProviderSpec, Suite};

/// Exit code: success (and no regression).
pub const EXIT_OK: i32 = 0;
/// Exit code: regression against the baseline, or an error.
pub const EXIT_FAIL: i32 = 1;
/// Exit code: invalid command line.
pub const EXIT_USAGE: i32 = 2;

/// Usage text.
pub const USAGE: &str = "\
usage: harness eval run SUITE [OPTIONS]
       harness eval compare BASELINE CURRENT [--json] [THRESHOLDS]

run options:
  --out FILE            results JSON (default: eval-results/<suite>-<time>.json)
  --parallel N          attempts at the same time (default 4)
  --filter TEXT         only tasks whose id contains TEXT
  --repeats N           attempts per task (overrides the suite)
  --model REF           run every task on this model instead of its fake script
  --keep-failed         keep the workspaces of failed attempts
  --in-process          run in-process with the fake provider (no subprocess)
  --shared-data-dir     attempts use the configured data dir instead of a
                        fresh one each (their sessions and runs are kept)
  --baseline FILE       compare with a stored baseline; exit 1 on regression
thresholds:
  --max-pass-drop PTS   allowed pass-rate drop in percentage points (default 0)
  --max-cost-rise PCT   allowed mean cost rise in percent (default 10)";

/// What the binary provides.
#[derive(Debug, Clone)]
pub struct Context {
    /// The `harness` executable used for attempts.
    pub harness_bin: PathBuf,
    /// `harness --version`.
    pub harness_version: String,
    /// `harness_config::effective::config_hash` of the current config.
    pub config_hash: String,
}

#[derive(Debug, Default)]
struct Args {
    positional: Vec<String>,
    out: Option<PathBuf>,
    parallel: Option<usize>,
    filter: Option<String>,
    repeats: Option<u32>,
    model: Option<String>,
    keep_failed: bool,
    in_process: bool,
    shared_data_dir: bool,
    baseline: Option<PathBuf>,
    json: bool,
    thresholds: Thresholds,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut val = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        let num = |s: String, flag: &str| -> Result<f64, String> {
            s.parse()
                .map_err(|_| format!("{flag}: `{s}` is not a number"))
        };
        match arg.as_str() {
            "--out" => a.out = Some(val("--out")?.into()),
            "--parallel" => a.parallel = Some(num(val("--parallel")?, "--parallel")? as usize),
            "--filter" => a.filter = Some(val("--filter")?),
            "--repeats" => a.repeats = Some(num(val("--repeats")?, "--repeats")? as u32),
            "--model" => a.model = Some(val("--model")?),
            "--baseline" => a.baseline = Some(val("--baseline")?.into()),
            "--max-pass-drop" => {
                a.thresholds.max_pass_rate_drop_pts =
                    num(val("--max-pass-drop")?, "--max-pass-drop")?
            }
            "--max-cost-rise" => {
                a.thresholds.max_cost_rise_pct = num(val("--max-cost-rise")?, "--max-cost-rise")?
            }
            "--keep-failed" => a.keep_failed = true,
            "--in-process" => a.in_process = true,
            "--shared-data-dir" => a.shared_data_dir = true,
            "--json" => a.json = true,
            f if f.starts_with("--") => return Err(format!("unknown option `{f}`")),
            _ => a.positional.push(arg.clone()),
        }
    }
    Ok(a)
}

/// Runs `harness eval ...` (`args` are the words after `eval`). Returns the
/// exit code.
pub async fn main(args: &[String], ctx: &Context, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let Some((cmd, rest)) = args.split_first() else {
        let _ = writeln!(err, "{USAGE}");
        return EXIT_USAGE;
    };
    let a = match parse(rest) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(err, "harness eval: {e}\n{USAGE}");
            return EXIT_USAGE;
        }
    };
    match cmd.as_str() {
        "run" => run(a, ctx, out, err).await,
        "compare" => compare_cmd(&a, out, err),
        "-h" | "--help" | "help" => {
            let _ = writeln!(out, "{USAGE}");
            EXIT_OK
        }
        other => {
            let _ = writeln!(err, "harness eval: unknown command `{other}`\n{USAGE}");
            EXIT_USAGE
        }
    }
}

/// The subprocess executor for `harness eval run`: a fresh data directory
/// per attempt unless `--shared-data-dir`.
fn process_executor(a: &Args, ctx: &Context) -> ProcessExecutor {
    ProcessExecutor::new(&ctx.harness_bin).with_shared_data_dir(a.shared_data_dir)
}

async fn run(a: Args, ctx: &Context, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let [suite_path] = a.positional.as_slice() else {
        let _ = writeln!(err, "harness eval run: expected one SUITE file\n{USAGE}");
        return EXIT_USAGE;
    };
    let mut suite = match Suite::load(suite_path.as_ref()) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(err, "harness eval: {e}");
            return EXIT_FAIL;
        }
    };
    if let Some(m) = &a.model {
        suite.defaults.provider = None;
        for t in &mut suite.tasks {
            t.provider = Some(ProviderSpec {
                fake: None,
                model: Some(m.clone()),
            });
        }
    }
    let executor: Arc<dyn Executor> = if a.in_process {
        Arc::new(InProcessExecutor::new())
    } else {
        Arc::new(process_executor(&a, ctx))
    };
    let opts = RunOptions {
        parallel: a.parallel.unwrap_or(4),
        filter: a.filter.clone(),
        repeats: a.repeats,
        keep_failed: a.keep_failed,
        stamp: Stamp {
            harness_version: ctx.harness_version.clone(),
            config_hash: ctx.config_hash.clone(),
        },
    };
    let results = run_suite(Arc::new(suite), executor, &opts, &CancellationToken::new()).await;
    let path = a.out.clone().unwrap_or_else(|| {
        let t = results.started_at;
        PathBuf::from("eval-results").join(format!(
            "{}-{:04}{:02}{:02}-{:02}{:02}{:02}.json",
            results.suite,
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        ))
    });
    let _ = write!(out, "{}", results.render());
    if let Err(e) = results.write(&path) {
        let _ = writeln!(err, "harness eval: writing {}: {e}", path.display());
        return EXIT_FAIL;
    }
    let _ = writeln!(out, "results: {}", path.display());
    match &a.baseline {
        None => EXIT_OK,
        Some(b) => match Results::load(b) {
            Ok(base) => report(&compare(&base, &results, &a.thresholds), a.json, out),
            Err(e) => {
                let _ = writeln!(err, "harness eval: {e}");
                EXIT_FAIL
            }
        },
    }
}

fn report(c: &crate::baseline::Comparison, json: bool, out: &mut dyn Write) -> i32 {
    if json {
        let _ = writeln!(out, "{}", c.to_json());
    } else {
        let _ = write!(out, "{}", c.render());
    }
    if c.passed { EXIT_OK } else { EXIT_FAIL }
}

fn compare_cmd(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let [base, cur] = a.positional.as_slice() else {
        let _ = writeln!(
            err,
            "harness eval compare: expected BASELINE and CURRENT\n{USAGE}"
        );
        return EXIT_USAGE;
    };
    let load = |p: &String| Results::load(p.as_ref());
    match (load(base), load(cur)) {
        (Ok(b), Ok(c)) => report(&compare(&b, &c, &a.thresholds), a.json, out),
        (Err(e), _) | (_, Err(e)) => {
            let _ = writeln!(err, "harness eval: {e}");
            EXIT_FAIL
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            harness_bin: "/nonexistent/harness".into(),
            harness_version: "0.1.0".into(),
            config_hash: "cafe".into(),
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn attempts_get_their_own_data_dir_unless_shared_is_asked_for() {
        let a = parse(&s(&["suite.yaml"])).unwrap();
        assert!(!process_executor(&a, &ctx()).shares_data_dir());
        let a = parse(&s(&["suite.yaml", "--shared-data-dir"])).unwrap();
        assert!(process_executor(&a, &ctx()).shares_data_dir());
        assert!(USAGE.contains("--shared-data-dir"));
    }

    #[tokio::test]
    async fn usage_errors() {
        let (mut o, mut e) = (Vec::new(), Vec::new());
        assert_eq!(main(&[], &ctx(), &mut o, &mut e).await, EXIT_USAGE);
        assert_eq!(main(&s(&["run"]), &ctx(), &mut o, &mut e).await, EXIT_USAGE);
        assert_eq!(
            main(&s(&["run", "x", "--bogus"]), &ctx(), &mut o, &mut e).await,
            EXIT_USAGE
        );
        assert_eq!(
            main(&s(&["frobnicate"]), &ctx(), &mut o, &mut e).await,
            EXIT_USAGE
        );
        assert_eq!(
            main(&s(&["run", "/missing.yaml"]), &ctx(), &mut o, &mut e).await,
            EXIT_FAIL
        );
    }

    #[tokio::test]
    async fn run_in_process_then_compare_against_itself() {
        let suite = format!("{}/suites/starter/suite.yaml", env!("CARGO_MANIFEST_DIR"));
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("r.json");
        let out_s = out_path.display().to_string();
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let code = main(
            &s(&[
                "run",
                &suite,
                "--in-process",
                "--filter",
                "false-done",
                "--out",
                &out_s,
            ]),
            &ctx(),
            &mut o,
            &mut e,
        )
        .await;
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&e));
        let r = Results::load(&out_path).unwrap();
        assert_eq!(r.stamp.config_hash, "cafe");
        assert_eq!(r.tasks.len(), 1);
        let (mut o, mut e) = (Vec::new(), Vec::new());
        assert_eq!(
            main(
                &s(&["compare", &out_s, &out_s, "--json"]),
                &ctx(),
                &mut o,
                &mut e
            )
            .await,
            EXIT_OK
        );
        let json: serde_json::Value = serde_json::from_slice(&o).unwrap();
        assert_eq!(json["passed"], true);
        // Baseline comparison inside `run`.
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let out2 = dir.path().join("r2.json").display().to_string();
        let code = main(
            &s(&[
                "run",
                &suite,
                "--in-process",
                "--filter",
                "false-done",
                "--out",
                &out2,
                "--baseline",
                &out_s,
            ]),
            &ctx(),
            &mut o,
            &mut e,
        )
        .await;
        assert_eq!(code, EXIT_OK);
        assert!(String::from_utf8_lossy(&o).contains("PASS"));
    }
}
