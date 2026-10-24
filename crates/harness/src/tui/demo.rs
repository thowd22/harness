//! `harness ui-demo`: a scripted session with no model or API key.
//!
//! The demo creates a throwaway project and data directory, an agent that
//! may delegate to the built-in researcher, and a fake-provider script, then
//! runs the real TUI and agent loop on it: streaming text and reasoning,
//! tool cards with diffs and live output, a subagent card, the goal bar fed
//! by the `todo` tool, a provider retry, an approval prompt (the bash call
//! asks in `auto-edit` mode; answer it with `y`), the map (Tab) and the
//! palette (Ctrl+P).
//!
//! - `--gallery` shows every component on its own page instead.
//! - `--print` plays the session without a terminal and prints the chat,
//!   map and palette screens as text (for checks and screenshots).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::EventStream;
use futures::StreamExt;
use harness_config::ini::Config;
use harness_config::paths::DataDir;
use harness_events::redact::Redactor;
use harness_policy::approvals::ChannelUi;
use harness_tui::app::{
    App, AppConfig, AppEvent, CoreEvent, CrosstermControl, TerminalSession, event_channel,
};
use harness_tui::demo::gallery::{Gallery, GalleryOutput};
use harness_tui::interaction::testing::{buffer_text, key};
use harness_tui::theme::Theme;
use ratatui::Terminal;
use ratatui::backend::{CrosstermBackend, TestBackend};
use tokio::sync::mpsc;

use super::{Driver, TuiOptions};

/// The demo's first message.
pub const PROMPT: &str = "Fix the panic in parse_header when a header has no colon.";

/// Name of the demo agent (the built-in coder plus a researcher subagent).
pub const AGENT: &str = "demo";

/// The scripted model: plan with `todo`, delegate to the researcher, edit,
/// hit a rate limit, run the checks (approval), finish.
pub const SCRIPT: &str = r#"responses:
  - chunk_delay_ms: 20
    content:
      - reasoning: { text: "Find where headers are parsed before changing anything." }
      - text: "I'll track the work and ask the researcher where the header parser lives."
      - tool_use: { name: todo, input: { action: add, id: t1, title: "Find the header parser", status: in_progress } }
      - tool_use: { name: todo, input: { action: add, id: t2, title: "Return an error instead of panicking" } }
      - tool_use: { name: spawn_agent, input: { agent: researcher, brief: "Find the function in src/ that splits HTTP headers at the colon and say what it does with a missing colon." } }
    usage: { input_tokens: 1200, cache_read_tokens: 9800, output_tokens: 240, reasoning_tokens: 60 }
  - chunk_delay_ms: 20
    content:
      - text: "`parse_header` in src/parse.rs calls `.unwrap()` on the colon split, so a header without a colon panics."
    usage: { input_tokens: 800, output_tokens: 60 }
  - chunk_delay_ms: 20
    content:
      - text: "Making `parse_header` return an error."
      - tool_use: { name: read, input: { path: src/parse.rs } }
      - tool_use: { name: edit, input: { path: src/parse.rs, old_string: ".unwrap()", new_string: ".ok_or(ParseError::MissingColon)?" } }
      - tool_use: { name: todo, input: { action: update, id: t1, status: done } }
      - tool_use: { name: todo, input: { action: update, id: t2, status: in_progress } }
    usage: { input_tokens: 400, cache_read_tokens: 11900, output_tokens: 120 }
  - error: { kind: rate_limited, message: slow down, retry_after_ms: 1500 }
  - chunk_delay_ms: 20
    content:
      - text: "Running the checks."
      - tool_use: { name: bash, input: { command: "sh check.sh" } }
      - tool_use: { name: todo, input: { action: update, id: t2, status: done } }
    usage: { input_tokens: 300, cache_read_tokens: 12300, output_tokens: 40 }
  - chunk_delay_ms: 20
    content:
      - text: "Done: `parse_header` now returns `ParseError::MissingColon` instead of panicking, and `check.sh` passes."
    usage: { input_tokens: 120, cache_read_tokens: 12400, output_tokens: 50 }
"#;

const PARSE_RS: &str = "pub enum ParseError {\n    MissingColon,\n}\n\npub fn parse_header(line: &str) -> Result<(&str, &str), ParseError> {\n    let (name, value) = line.split_once(':').unwrap();\n    Ok((name.trim(), value.trim()))\n}\n";
const CHECK_SH: &str = "#!/bin/sh\necho 'running 3 tests'\necho 'test parse::ok ... ok'\necho 'test parse::spaces ... ok'\necho 'test parse::missing_colon ... ok'\necho 'test result: ok. 3 passed'\n";
const AGENT_YAML: &str = "name: demo\ndescription: The coder with a researcher to delegate to (ui-demo)\nprompt: coder\ntools: [read, write, edit, bash, todo]\nsubagents: [researcher]\nmemory: off\n";

/// A temporary demo project, data directory and script.
struct Setup {
    _tmp: tempfile::TempDir,
    cfg: Config,
    data: DataDir,
    project: PathBuf,
    script: PathBuf,
}

fn setup() -> Result<Setup, String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = tmp.path().to_path_buf();
    let project = root.join("project");
    let write = |p: PathBuf, text: &str| -> Result<(), String> {
        std::fs::create_dir_all(p.parent().expect("has a parent")).map_err(|e| e.to_string())?;
        std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))
    };
    write(project.join("src/parse.rs"), PARSE_RS)?;
    write(project.join("check.sh"), CHECK_SH)?;
    write(
        project
            .join(harness_config::trust::PROJECT_CONFIG_DIR)
            .join("agents/demo.yaml"),
        AGENT_YAML,
    )?;
    let script = root.join("demo-script.yaml");
    write(script.clone(), SCRIPT)?;
    let mut cfg = Config::defaults_with_data_dir(root.join("data"));
    cfg.memory.enabled = false;
    // The scripted commands only touch the demo's temp project. Without the
    // sandbox the screens do not depend on the host (no Landlock warnings on
    // macOS or older kernels covering the cards).
    cfg.sandbox.enabled = false;
    let data = DataDir::from_config(&cfg)
        .open()
        .map_err(|e| e.to_string())?;
    trust(&data, &project)?;
    Ok(Setup {
        _tmp: tmp,
        cfg,
        data,
        project,
        script,
    })
}

/// The demo created the project, so it trusts its `.harness/` directory.
fn trust(data: &DataDir, project: &Path) -> Result<(), String> {
    let store = harness_config::trust::TrustStore::from_data_dir(data);
    let snapshot = harness_config::trust::snapshot_project(project)
        .map_err(|e| e.to_string())?
        .ok_or("the demo project has no .harness directory")?;
    store.trust(project, &snapshot).map_err(|e| e.to_string())
}

fn options(s: &Setup) -> TuiOptions {
    TuiOptions {
        agent: Some(AGENT.into()),
        cwd: Some(s.project.clone()),
        fake_script: Some(s.script.clone()),
        prompt: Some(PROMPT.into()),
        ..TuiOptions::default()
    }
}

/// `harness ui-demo [--gallery | --print]`.
pub fn run(args: &[String]) -> ExitCode {
    let rt = crate::runtime();
    let result = match args.first().map(String::as_str) {
        None => rt.block_on(interactive()),
        Some("--gallery") => rt.block_on(gallery()),
        Some("--print") => rt.block_on(print(&mut std::io::stdout())),
        Some(other) => Err(format!(
            "unknown option `{other}`; usage: harness ui-demo [--gallery | --print]"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("harness ui-demo: {e}");
            ExitCode::from(crate::cli::exit::ERROR)
        }
    }
}

async fn interactive() -> Result<(), String> {
    let s = setup()?;
    harness_tui::terminal::install_panic_hook();
    let rc = super::run(
        options(&s),
        s.cfg.clone(),
        s.data.clone(),
        Vec::new(),
        Arc::new(Redactor::default()),
    )
    .await;
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("exit code {rc}"))
    }
}

async fn gallery() -> Result<(), String> {
    harness_tui::terminal::install_panic_hook();
    let session =
        TerminalSession::enter(CrosstermControl::default(), true).map_err(|e| e.to_string())?;
    let mut term =
        Terminal::new(CrosstermBackend::new(std::io::stdout())).map_err(|e| e.to_string())?;
    let mut g = Gallery::new(Theme::default());
    let mut events = EventStream::new();
    loop {
        term.draw(|f| g.render(f, f.area()))
            .map_err(|e| e.to_string())?;
        match events.next().await {
            Some(Ok(ev)) => {
                if g.handle(&ev) == Some(GalleryOutput::Quit) {
                    break;
                }
            }
            Some(Err(e)) => return Err(e.to_string()),
            None => break,
        }
    }
    session.finish().map_err(|e| e.to_string())
}

/// Plays the session without a terminal and prints the chat after the
/// run, the map and the palette.
pub async fn print(out: &mut dyn Write) -> Result<(), String> {
    let s = setup()?;
    let (tx, mut rx) = event_channel();
    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel();
    let (approvals, mut prompts) = ChannelUi::new(8);
    let mut driver = Driver::new(
        s.cfg.clone(),
        s.data.clone(),
        Vec::new(),
        Arc::new(Redactor::default()),
        tx.clone(),
        approvals,
    );
    driver.start(options(&s)).await?;
    let app_cfg = AppConfig {
        animations: false,
        ..AppConfig::default()
    }
    .with_dirs(&s.project, None);
    let mut app = App::new(app_cfg, ui_tx);
    let mut term = Terminal::new(TestBackend::new(120, 44)).map_err(|e| e.to_string())?;
    let draw = |app: &mut App, term: &mut Terminal<TestBackend>| -> String {
        let _ = term.draw(|f| app.render(f));
        buffer_text(term.backend().buffer())
    };
    // Play until the turn ends; approve the prompt the way a user would.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut approved = false;
    loop {
        if driver.turn.as_ref().is_some_and(|t| t.is_finished()) {
            let (core, outcome) = driver
                .turn
                .take()
                .expect("turn")
                .await
                .map_err(|e| e.to_string())?;
            driver.turn_done(core, outcome).await;
            break;
        }
        tokio::select! {
            ev = rx.recv() => match ev {
                Some(ev) => app.handle(ev),
                None => break,
            },
            env = prompts.recv() => if let Some(env) = env {
                app.handle(AppEvent::Core(CoreEvent::Approval(env)));
                draw(&mut app, &mut term);
                app.handle(AppEvent::Terminal(key("y")));
                approved = true;
            },
            r = ui_rx.recv() => if let Some(r) = r {
                driver.request(r).await;
            },
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        if tokio::time::Instant::now() > deadline {
            return Err("the demo did not finish".into());
        }
    }
    // Drain what is left (hygiene, final events).
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
        app.handle(ev);
    }
    let chat = draw(&mut app, &mut term);
    app.handle(AppEvent::Terminal(key("tab")));
    let map = draw(&mut app, &mut term);
    app.handle(AppEvent::Terminal(key("tab")));
    app.handle(AppEvent::Terminal(key("ctrl+p")));
    let palette = draw(&mut app, &mut term);
    for (title, screen) in [("chat", chat), ("map", map), ("palette", palette)] {
        let _ = writeln!(out, "=== {title} ===\n{screen}");
    }
    let _ = writeln!(
        out,
        "=== approval ===\n{}",
        if approved { "answered: once" } else { "none" }
    );
    driver.finish_core().await;
    Ok(())
}
