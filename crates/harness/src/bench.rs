//! `harness bench`: runs the bundled multi-turn coding task with the timing
//! probe installed and prints a latency report (see `docs/performance.md`).

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use harness_agent::budget::DenyOverruns;
use harness_config::ini::Config;
use harness_config::paths::DataDir;
use harness_eval::bench::{
    self, BenchReport, Machine, NetProbe, RenderStats, Stat, ToolSchema, scenario,
};
use harness_events::redact::Redactor;
use harness_events::schema::RunEvent;
use harness_events::store::read_run;
use harness_providers::timing;
use tokio_util::sync::CancellationToken;

use crate::cli::{Approvals, exit};
use crate::core::{Core, CoreOptions};
use crate::headless::{HeadlessUi, parse_mode};

/// Usage of `harness bench`.
pub const USAGE: &str = "\
usage: harness bench (--fake | --model REF) [OPTIONS]

Runs the bundled coding task (a small Python library with a bug: fix it,
then add a method with a test) in a fresh temporary git repository and
data directory, and reports per model call and per turn: time to first
byte and token, output tokens/s, call time, tool time, request hooks and
the harness overhead between calls, cache use and request sizes.

Modes:
  --fake                 scripted fake provider: offline, deterministic,
                         measures harness overhead only
  --model REF            a real model (alias or provider/model); costs money

Options:
  --runs N               sessions to run (default 1; fake: 5)
  --label TEXT           configuration label stored in the report
  --json FILE            also write the JSON report (`-` = stdout only)
  --budget-usd USD       cost limit per turn (live default 1.00)
  --max-turns N          model-call limit per turn (default 20)
  --mode MODE            autonomy mode (default auto)
  --checkpoints          snapshot the project before each turn (as the TUI does)
  --turns N              run only the first N prompts
  --no-render            skip the TUI render replay
  --net                  measure cold and warm connection setup to the
                         provider (a few GETs, no tokens)
  --keep                 keep the temporary repositories and data dirs
  --compaction           (with --fake) the same task in a 20k-token window
                         that compacts once, with slow scripted calls; shows
                         the turn time spent waiting for compaction (compare
                         [context] background_compaction off and on)
  -h, --help             print this help";

#[derive(Debug, Default)]
struct Args {
    fake: bool,
    model: Option<String>,
    runs: Option<u32>,
    label: Option<String>,
    json: Option<String>,
    budget_usd: Option<f64>,
    max_turns: Option<u32>,
    mode: Option<String>,
    checkpoints: bool,
    turns: Option<usize>,
    no_render: bool,
    net: bool,
    keep: bool,
    compaction: bool,
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
            "--fake" => a.fake = true,
            "--model" => a.model = Some(val("--model")?),
            "--runs" => a.runs = Some(num(val("--runs")?, "--runs")?.max(1.0) as u32),
            "--label" => a.label = Some(val("--label")?),
            "--json" => a.json = Some(val("--json")?),
            "--budget-usd" => a.budget_usd = Some(num(val("--budget-usd")?, "--budget-usd")?),
            "--max-turns" => a.max_turns = Some(num(val("--max-turns")?, "--max-turns")? as u32),
            "--mode" => a.mode = Some(val("--mode")?),
            "--turns" => a.turns = Some(num(val("--turns")?, "--turns")? as usize),
            "--checkpoints" => a.checkpoints = true,
            "--no-render" => a.no_render = true,
            "--net" => a.net = true,
            "--keep" => a.keep = true,
            "--compaction" => a.compaction = true,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if a.fake == a.model.is_some() {
        return Err("give exactly one of --fake and --model".into());
    }
    if a.compaction && !a.fake {
        return Err("--compaction needs --fake".into());
    }
    Ok(a)
}

/// `harness bench`. Returns the exit code.
pub async fn main(
    args: &[String],
    cfg: Config,
    secrets: Vec<String>,
    redactor: Arc<Redactor>,
    out: &mut dyn Write,
) -> u8 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        let _ = writeln!(out, "{USAGE}");
        return exit::SUCCESS;
    }
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("harness bench: {e}\n\n{USAGE}");
            return exit::USAGE;
        }
    };
    match run(&a, cfg, secrets, redactor, out).await {
        Ok(()) => exit::SUCCESS,
        Err(e) => {
            eprintln!("harness bench: {e}");
            exit::ERROR
        }
    }
}

struct RunOutput {
    events: Vec<RunEvent>,
    resolved: Vec<RunEvent>,
    tool_schemas: Vec<ToolSchema>,
    report: bench::RunReport,
    model: String,
}

async fn run(
    a: &Args,
    cfg: Config,
    secrets: Vec<String>,
    redactor: Arc<Redactor>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let probe = timing::install();
    let runs = a.runs.unwrap_or(if a.fake { 5 } else { 1 });
    let mut prompts = scenario::prompts();
    if let Some(n) = a.turns {
        prompts.truncate(n.max(1));
    }
    let mut report = BenchReport {
        schema: bench::BENCH_SCHEMA,
        mode: if a.fake { "fake" } else { "live" }.into(),
        config: a.label.clone().unwrap_or_else(|| "default".into()),
        machine: Machine::detect(env!("CARGO_PKG_VERSION")),
        ..BenchReport::default()
    };
    let mut last = None;
    for n in 1..=runs {
        let label = format!("{}{n}", report.config);
        let o = one_run(a, &cfg, &secrets, &redactor, &prompts, &label).await?;
        report.model = o.model.clone();
        report.runs.push(o.report.clone());
        eprintln!(
            "harness bench: run {label} done in {:.1}s",
            o.report.wall_ms / 1000.0
        );
        last = Some(o);
    }
    probe.clear();
    report.summary = bench::summarize(&report.runs);
    if let Some(o) = last {
        report.tool_schemas = o.tool_schemas;
        report.emit = bench::emit_costs(&o.resolved, redactor.clone()).await;
        if !a.no_render {
            report.render = Some(render_replay(&o.events));
        }
    }
    if a.net && !a.fake {
        report.net = net_probe(&cfg, a.model.as_deref().unwrap_or_default()).await;
    }
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    match a.json.as_deref() {
        Some("-") => {
            let _ = writeln!(out, "{json}");
        }
        Some(path) => {
            std::fs::write(path, &json).map_err(|e| format!("{path}: {e}"))?;
            let _ = write!(out, "{}", bench::render_table(&report));
        }
        None => {
            let _ = write!(out, "{}", bench::render_table(&report));
        }
    }
    Ok(())
}

async fn one_run(
    a: &Args,
    cfg: &Config,
    secrets: &[String],
    redactor: &Arc<Redactor>,
    prompts: &[String],
    label: &str,
) -> Result<RunOutput, String> {
    let probe = timing::install();
    let repo = tempfile::Builder::new()
        .prefix("harness-bench-repo-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let data_tmp = tempfile::Builder::new()
        .prefix("harness-bench-data-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    scenario::write_repo(repo.path()).map_err(|e| format!("bench repo: {e}"))?;
    let fake_script = if a.fake {
        let p = data_tmp.path().join("bench-fake.yaml");
        let script = if a.compaction {
            scenario::COMPACTION_SCRIPT
        } else {
            scenario::FAKE_SCRIPT
        };
        std::fs::write(&p, script).map_err(|e| e.to_string())?;
        Some(p)
    } else {
        None
    };
    let data = DataDir::new(data_tmp.path().join("data"))
        .open()
        .map_err(|e| format!("data directory: {e}"))?;
    let mode = parse_mode(a.mode.as_deref().unwrap_or("auto"))?;
    let opts = CoreOptions {
        cwd: Some(repo.path().to_owned()),
        agent: None,
        model: a.model.clone(),
        mode: Some(mode),
        fake_script,
        max_turns: Some(a.max_turns.unwrap_or(20)),
        budget_tokens: None,
        budget_usd: a.budget_usd.or(if a.fake { None } else { Some(1.0) }),
        verify: true,
        headless: true,
        approvals: Arc::new(HeadlessUi::new(Approvals::Deny)),
        allow: Vec::new(),
        budget_approver: Arc::new(DenyOverruns),
        resume: None,
        title: prompts.first().cloned().unwrap_or_default(),
        checkpoints: a.checkpoints,
    };
    probe.clear();
    let started = Instant::now();
    let mut core = Core::start(cfg.clone(), data, secrets.to_vec(), redactor.clone(), opts).await?;
    probe.span(bench::START_SPAN, started);
    for w in &core.warnings {
        eprintln!("harness bench: warning: {w}");
    }
    let tool_schemas = core
        .tool_specs()
        .iter()
        .map(|t| {
            let bytes = serde_json::to_string(t).map_or(0, |s| s.len() as u64);
            ToolSchema {
                name: t.name.clone(),
                bytes,
                tokens_est: bytes / 4,
            }
        })
        .collect();
    let model = core.model.reference.clone();
    core.begin();
    // As an interactive session does while the user types
    // (`[experimental] cache_prewarm`); the first turn waits for it.
    core.spawn_prewarm("session_start");
    let cancel = CancellationToken::new();
    for (i, prompt) in prompts.iter().enumerate() {
        let start = Instant::now();
        let outcome = core.turn(prompt.clone(), &cancel).await;
        probe.span(format!("{}{}", bench::TURN_SPAN, i + 1), start);
        if let Some(e) = crate::core::finished_error(&outcome) {
            eprintln!("harness bench: run {label} turn {}: {e}", i + 1);
        }
    }
    core.wait_summary(&cancel).await;
    let dir = core.recorder.dir().to_owned();
    let blobs = core.recorder.blobs().clone();
    core.finish().await?;
    let data = probe.snapshot();
    let events = read_run(&dir).map_err(|e| e.to_string())?.events;
    let resolved = events
        .iter()
        .cloned()
        .map(|mut e| {
            for p in e.kind.payloads_mut() {
                if let Ok(inline) = blobs.resolve(p) {
                    *p = inline;
                }
            }
            e
        })
        .collect();
    let report = bench::analyze(label, &data, &events);
    if a.keep {
        let (r, d) = (repo.keep(), data_tmp.keep());
        eprintln!("harness bench: kept {} and {}", r.display(), d.display());
    }
    Ok(RunOutput {
        events,
        resolved,
        tool_schemas,
        report,
        model,
    })
}

/// Replays `events` into the TUI on a 120x40 test backend, drawing every
/// dirty frame, and times the draws and event handling.
fn render_replay(events: &[RunEvent]) -> RenderStats {
    use harness_tui::app::{App, AppConfig, AppEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let size = (120u16, 40u16);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), tx);
    let Ok(mut term) = Terminal::new(TestBackend::new(size.0, size.1));
    let (mut frames, mut handles) = (Vec::new(), Vec::new());
    for e in events {
        let start = Instant::now();
        app.handle(AppEvent::Run(Box::new(e.clone())));
        handles.push(start.elapsed().as_secs_f64() * 1000.0);
        if app.is_dirty() {
            let start = Instant::now();
            let _ = term.draw(|f| app.render(f));
            frames.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    RenderStats {
        size,
        events: events.len() as u64,
        frames: frames.len() as u64,
        frame_ms: Stat::of(&frames),
        handle_ms: Stat::of(&handles),
    }
}

/// Cold (new isolated client), warm (same client) and new provider client
/// (a fresh client on the shared pool, as a subagent gets) time to
/// response headers against the model's provider.
async fn net_probe(cfg: &Config, model: &str) -> Option<NetProbe> {
    use harness_providers::http::{HttpClient, HttpConfig};
    let registry = crate::wiring::registry(cfg).ok()?;
    let resolved = registry.resolve(model).ok()?;
    let (base, path) = match resolved.reference.provider.as_str() {
        "openrouter" => (harness_providers::openrouter::DEFAULT_BASE_URL, "key"),
        "anthropic" => ("https://api.anthropic.com", "v1/models"),
        "openai" => ("https://api.openai.com", "v1/models"),
        _ => return None,
    };
    let base = cfg
        .providers
        .get(resolved.reference.provider.as_str())
        .and_then(|p| p.base_url.clone())
        .unwrap_or_else(|| base.to_owned());
    let mut probe = NetProbe {
        url: format!("{base}/{path}"),
        ..NetProbe::default()
    };
    let mut warm_client = None;
    for _ in 0..3 {
        let client = HttpClient::isolated(HttpConfig::new(base.clone())).ok()?;
        let (version, took) = client.warm(path).await.ok()?;
        probe.version = version;
        probe.cold_ms.push(took.as_secs_f64() * 1000.0);
        warm_client = Some(client);
    }
    let client = warm_client?;
    for _ in 0..3 {
        let (_, took) = client.warm(path).await.ok()?;
        probe.warm_ms.push(took.as_secs_f64() * 1000.0);
    }
    // The shared pool for `base` is warm from the session that just ran.
    for _ in 0..3 {
        let fresh = HttpClient::new(HttpConfig::new(base.clone())).ok()?;
        let (_, took) = fresh.warm(path).await.ok()?;
        probe.new_client_ms.push(took.as_secs_f64() * 1000.0);
    }
    Some(probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn compaction_variant_needs_the_fake_provider() {
        let a = parse(&args(&["--fake", "--compaction"])).unwrap();
        assert!(a.fake && a.compaction);
        assert!(!parse(&args(&["--fake"])).unwrap().compaction);
        let e = parse(&args(&["--model", "opus", "--compaction"])).unwrap_err();
        assert!(e.contains("--compaction needs --fake"), "{e}");
    }
}
