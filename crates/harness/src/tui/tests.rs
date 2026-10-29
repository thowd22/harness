//! The TUI driver in-process: real core, fake provider, the UI side
//! replaced by the request and event channels.

use std::path::{Path, PathBuf};
use std::time::Duration;

use harness_events::schema::EventKind;
use harness_policy::approvals::{ApprovalAnswer, ApprovalScope};
use harness_tui::palette::Invocation;

use super::*;

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        Env {
            _tmp: tmp,
            root,
            project,
        }
    }

    fn script(&self, yaml: &str) -> PathBuf {
        let p = self.root.join(format!("script-{}.yaml", rand_suffix()));
        std::fs::write(&p, yaml).unwrap();
        p
    }
}

fn rand_suffix() -> String {
    harness_events::schema::NodeId::random().as_str().to_owned()
}

struct Harness {
    d: Driver,
    rx: mpsc::Receiver<AppEvent>,
    prompts: mpsc::Receiver<PromptEnvelope>,
    seen: Vec<AppEvent>,
    /// Finished browser sign-ins, handled as the driver loop does.
    sign_ins: mpsc::UnboundedReceiver<super::signin::Finished>,
}

impl Harness {
    async fn start(env: &Env, script: &Path) -> Harness {
        Harness::with(env, script, false).await
    }

    async fn with(env: &Env, script: &Path, memory: bool) -> Harness {
        Harness::with_ini(env, script, memory, "").await
    }

    async fn with_ini(env: &Env, script: &Path, memory: bool, ini: &str) -> Harness {
        let mut cfg = harness_config::ini::load_str(ini, None).unwrap();
        cfg.general.data_dir = env.root.join("data");
        cfg.memory.enabled = memory;
        // The sandbox launcher is the `harness` binary; unit tests run
        // inside the test executable instead (the binary tests cover it).
        cfg.sandbox.enabled = false;
        let data = harness_config::paths::DataDir::from_config(&cfg)
            .open()
            .unwrap();
        let (tx, rx) = event_channel();
        let (approvals, prompts) = ChannelUi::new(8);
        let mut d = Driver::new(
            cfg,
            data,
            Vec::new(),
            Arc::new(Redactor::default()),
            tx,
            approvals,
        );
        d.start(TuiOptions {
            cwd: Some(env.project.clone()),
            fake_script: Some(script.to_path_buf()),
            ..TuiOptions::default()
        })
        .await
        .unwrap();
        // A desktop session: sign-ins open the (mock) browser here.
        d.sign_ins.env = harness_providers::signin::Environment::from_env(&|k| {
            (k == "DISPLAY").then(|| ":0".to_owned())
        });
        let sign_ins = d.sign_ins.take_done();
        Harness {
            d,
            rx,
            prompts,
            seen: Vec::new(),
            sign_ins,
        }
    }

    /// Waits for the running turn and hands the core back.
    async fn finish_turn(&mut self) {
        if let Some(t) = self.d.turn.take() {
            let (core, outcome) = t.await.unwrap();
            self.d.turn_done(core, outcome).await;
        }
    }

    /// Collects events until `pred` matches one (or times out).
    async fn until(&mut self, mut pred: impl FnMut(&AppEvent) -> bool) -> &AppEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(i) = self.seen.iter().position(&mut pred) {
                return &self.seen[i];
            }
            let next = tokio::time::timeout_at(deadline, async {
                tokio::select! {
                    ev = self.rx.recv() => Some(ev),
                    Some(f) = self.sign_ins.recv() => {
                        super::signin::finished(&mut self.d, f).await;
                        None
                    }
                }
            })
            .await;
            match next {
                Ok(Some(Some(ev))) => self.seen.push(ev),
                Ok(None) => {}
                _ => panic!("event not seen; got {:?}", self.seen),
            }
        }
    }

    async fn panel(&mut self, title: &str) -> Vec<String> {
        let t = title.to_owned();
        match self
            .until(|e| matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if *title == t))
            .await
        {
            AppEvent::Core(CoreEvent::Panel { lines, .. }) => lines.clone(),
            _ => unreachable!(),
        }
    }
}

const TEXT: &str = "responses:\n  - content:\n      - text: \"All done.\"\n";

/// Regression: `/new` reset the UI before the old session finished, so the
/// old run's `run_finished` arrived after the reset and the UI adopted the
/// old run's id; the new run's palette entry then stayed at "0ms".
#[tokio::test]
async fn new_session_events_never_mix_with_the_old_run() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("first".into())).await;
    h.finish_turn().await;
    let old = h.d.core.as_ref().unwrap().recorder.run_id();
    h.seen.clear();

    h.d.request(UiRequest::Submit("/new".into())).await;
    let new = h.d.core.as_ref().unwrap().recorder.run_id();
    assert_ne!(old, new);
    h.until(|e| {
        matches!(e, AppEvent::Run(ev)
            if ev.run_id == new && matches!(ev.kind, EventKind::RunStarted { .. }))
    })
    .await;

    let reset = h
        .seen
        .iter()
        .position(|e| matches!(e, AppEvent::Core(CoreEvent::Reset)))
        .expect("the UI is reset");
    for (i, e) in h.seen.iter().enumerate() {
        if let AppEvent::Run(ev) = e {
            if i > reset {
                assert_eq!(
                    ev.run_id, new,
                    "old-run event after the reset: {:?}",
                    ev.kind
                );
            } else {
                assert_eq!(
                    ev.run_id, old,
                    "new-run event before the reset: {:?}",
                    ev.kind
                );
            }
        }
    }
    // The old run's end reached the UI before the reset.
    assert!(h.seen[..reset].iter().any(|e| matches!(e, AppEvent::Run(ev)
        if ev.run_id == old && matches!(ev.kind, EventKind::RunFinished { .. }))));
}

#[tokio::test]
async fn turns_stream_events_and_queue_while_busy() {
    let env = Env::new();
    let script = env.script(
        "responses:\n  - delay_ms: 200\n    content: [ { text: \"first answer\" } ]\n  - content: [ { text: \"second answer\" } ]\n",
    );
    let mut h = Harness::start(&env, &script).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Session { .. })))
        .await;
    h.d.request(UiRequest::Submit("one".into())).await;
    assert!(h.d.busy());
    h.d.request(UiRequest::Submit("two".into())).await;
    assert_eq!(h.d.queue.len(), 1, "queued while busy");
    h.finish_turn().await;
    // The queued prompt starts right after.
    assert!(h.d.busy());
    h.finish_turn().await;
    for want in ["first answer", "second answer"] {
        h.until(|e| {
            matches!(e, AppEvent::Run(ev) if matches!(&ev.kind, EventKind::Delta { text, .. } if text.contains(want)))
        })
        .await;
    }
    h.until(|e| {
        matches!(e, AppEvent::Run(ev) if matches!(&ev.kind, EventKind::UserMessage { text } if text == "two"))
    })
    .await;
}

#[tokio::test]
async fn interrupt_cancels_the_running_turn() {
    let env = Env::new();
    let script = env.script("responses:\n  - delay_ms: 30000\n    content: [ { text: late } ]\n");
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("go".into())).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.d.request(UiRequest::Interrupt).await;
    let t = h.d.turn.take().unwrap();
    let (core, outcome) = tokio::time::timeout(Duration::from_secs(10), t)
        .await
        .expect("cancelled promptly")
        .unwrap();
    assert_eq!(
        outcome.stop,
        harness_agent::agent_loop::StopReason::Cancelled
    );
    h.d.turn_done(core, outcome).await;
}

#[tokio::test]
async fn builtin_commands_open_panels() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("/help".into())).await;
    let help = h.panel("Help").await;
    assert!(help.iter().any(|l| l.contains("/cost")), "{help:?}");
    h.d.request(UiRequest::Submit("/config".into())).await;
    let cfg = h.panel("Effective config").await;
    assert!(cfg.iter().any(|l| l.contains("default_model")), "{cfg:?}");
    h.d.request(UiRequest::Submit("/cost".into())).await;
    let cost = h.panel("Cost").await;
    assert!(cost[0].starts_with("total:"), "{cost:?}");
    h.d.request(UiRequest::Submit("/remote".into())).await;
    let remote = h.panel(commands::REMOTE_TITLE).await;
    assert!(remote[0].contains("no remote hosts"), "{remote:?}");
    h.d.request(UiRequest::Submit("/context".into())).await;
    let ctx = h.panel("Context").await;
    assert!(ctx[0].contains("tokens before history"), "{ctx:?}");
    let identity_tokens =
        harness_tui::inspector::estimate_tokens(harness_config::prompts::IDENTITY);
    assert!(
        ctx.iter()
            .any(|l| l.ends_with("identity layer") && l.contains(&identity_tokens.to_string())),
        "{ctx:?}"
    );
    // The palette entry of a built-in runs the same command.
    h.d.request(UiRequest::Run(Invocation {
        id: "cmd.agents".into(),
        args: vec![],
    }))
    .await;
    let agents = h.panel("Agents").await;
    assert!(
        agents
            .iter()
            .any(|l| l.starts_with("Coder") && l.contains("(/coder · ")),
        "{agents:?}"
    );
    h.d.request(UiRequest::Submit("/nope".into())).await;
    h.until(|e| {
        matches!(
            e,
            AppEvent::Core(CoreEvent::Notify {
                level: Level::Error,
                ..
            })
        )
    })
    .await;
}

#[tokio::test]
async fn palette_lists_every_builtin_with_description_and_argument_help() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::start(&env, &script).await;
    let entries = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::PaletteEntries(_))))
        .await
    {
        AppEvent::Core(CoreEvent::PaletteEntries(v)) => v.clone(),
        _ => unreachable!(),
    };
    for b in harness_agent::commands::Builtin::ALL {
        let e = entries
            .iter()
            .find(|e| e.id == format!("cmd.{}", b.name()))
            .unwrap_or_else(|| panic!("/{} missing from the palette", b.name()));
        assert_eq!(e.description, b.description());
        assert_eq!(e.args.len(), b.args().len(), "/{}", b.name());
        for a in &e.args {
            assert!(
                a.hint.is_some(),
                "/{} {} has argument help",
                b.name(),
                a.name
            );
        }
    }
    let mode = entries.iter().find(|e| e.id == "cmd.mode").unwrap();
    assert!(
        mode.title.contains("ask"),
        "usage in the title: {}",
        mode.title
    );
    let slash = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::SlashCommands(_))))
        .await
    {
        AppEvent::Core(CoreEvent::SlashCommands(v)) => v.clone(),
        _ => unreachable!(),
    };
    assert!(slash.iter().any(|c| c.name == "fix" && c.scope.is_some()));
}

#[tokio::test]
async fn agents_show_by_display_name_in_the_palette_and_the_agents_panel() {
    let env = Env::new();
    let agents = env.project.join(".harness/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("deep-research.yaml"),
        "name: deep-research\ndisplay_name: Deep Diver\ndescription: Reads widely\nprompt_text: read\n",
    )
    .unwrap();
    // Trust the project as `harness trust` would, in the driver's data dir.
    let mut cfg = harness_config::ini::load_str("", None).unwrap();
    cfg.general.data_dir = env.root.join("data");
    let data = harness_config::paths::DataDir::from_config(&cfg)
        .open()
        .unwrap();
    let snap = harness_config::trust::snapshot_project(&env.project)
        .unwrap()
        .unwrap();
    harness_config::trust::TrustStore::from_data_dir(&data)
        .trust(&env.project, &snap)
        .unwrap();
    let script = env.script(TEXT);
    let mut h = Harness::start(&env, &script).await;
    let entries = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::PaletteEntries(_))))
        .await
    {
        AppEvent::Core(CoreEvent::PaletteEntries(v)) => v.clone(),
        _ => unreachable!(),
    };
    let deep = entries
        .iter()
        .find(|e| e.id == "cmd.deep-research")
        .unwrap_or_else(|| panic!("custom agent in the palette: {entries:?}"));
    assert_eq!(deep.description, "Switch to Deep Diver: Reads widely");
    let coder = entries.iter().find(|e| e.id == "cmd.coder").unwrap();
    assert!(
        coder.description.starts_with("Switch to Coder: "),
        "{}",
        coder.description
    );
    h.d.request(UiRequest::Submit("/agents".into())).await;
    let lines = h.panel("Agents").await;
    assert!(
        lines.iter().any(|l| l.starts_with("Deep Diver ")
            && l.contains("Reads widely")
            && l.contains("(/deep-research · ")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn past_runs_are_listed_by_title_never_by_id() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::start(&env, &script).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Session { .. })))
        .await;
    h.d.request(UiRequest::Submit(
        "Fix the header parser\nIt drops values".into(),
    ))
    .await;
    h.finish_turn().await;
    h.d.request(UiRequest::Submit("/runs".into())).await;
    let runs = h.panel("Runs").await;
    assert!(
        runs.iter()
            .any(|l| l.starts_with("Coder · Fix the header parser · ") && l.contains(" · ◐")),
        "{runs:?}"
    );
    for l in &runs {
        assert!(harness_events::names::find_raw_id(l).is_none(), "{l}");
    }
    let core = h.d.core.as_ref().expect("core is back after the turn");
    let entries = commands::palette_entries(&h.d.registry, core);
    let run = entries
        .iter()
        .find(|e| e.kind == harness_tui::palette::EntryKind::Run)
        .expect("a run entry");
    assert!(
        run.title.starts_with("Coder · Fix the header parser · "),
        "{}",
        run.title
    );
    assert!(
        run.description.starts_with("Running"),
        "{}",
        run.description
    );
    assert!(harness_events::names::find_raw_id(&run.title).is_none());
    assert!(harness_events::names::find_raw_id(&run.description).is_none());
    // Opening it shows the run by name, without its raw id.
    let (id, title) = (run.id.clone(), run.title.clone());
    h.d.request(UiRequest::Run(Invocation {
        id: id.clone(),
        args: vec![],
    }))
    .await;
    let (panel_title, lines) = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if title.starts_with("Run #")))
        .await
    {
        AppEvent::Core(CoreEvent::Panel { title, lines, .. }) => (title.clone(), lines.clone()),
        _ => unreachable!(),
    };
    assert_eq!(
        panel_title,
        format!(
            "Run {}",
            harness_events::names::short_tag(&id[harness_tui::palette::ids::RUN_PREFIX.len()..])
        )
    );
    assert!(!title.contains('#'), "a lone run needs no tag: {title}");
    assert!(
        lines[0].starts_with("Coder · Fix the header parser"),
        "{lines:?}"
    );
    for l in lines.iter().chain([&panel_title]) {
        assert!(harness_events::names::find_raw_id(l).is_none(), "{l}");
    }
}

#[tokio::test]
async fn undo_and_diff_use_the_turn_checkpoint() {
    let env = Env::new();
    std::fs::write(env.project.join("keep.txt"), "kept\n").unwrap();
    let script = env.script(
        "responses:\n  - content:\n      - tool_use: { name: write, input: { path: made.txt, content: \"new\\n\" } }\n  - content: [ { text: done } ]\n",
    );
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("make a file".into())).await;
    h.finish_turn().await;
    assert!(env.project.join("made.txt").exists());
    // The diff shows the change and can revert it.
    h.d.request(UiRequest::OpenDiff(None)).await;
    match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::Diff(_))))
        .await
    {
        AppEvent::Core(CoreEvent::Diff(spec)) => {
            assert!(spec.reverter.is_some(), "revertable to the turn checkpoint")
        }
        _ => unreachable!(),
    }
    // The change ledger reaches node detail.
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Hygiene { ledger, .. }) if ledger.iter().any(|l| l.contains("made.txt"))))
        .await;
    h.d.request(UiRequest::Submit("/undo".into())).await;
    h.until(
        |e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "restored"),
    )
    .await;
    assert!(
        !env.project.join("made.txt").exists(),
        "undo restored the turn"
    );
    assert!(env.project.join("keep.txt").exists());
}

#[tokio::test]
async fn mode_changes_reach_the_policy_gate() {
    let env = Env::new();
    let script = env.script(
        "responses:\n  - content: [ { tool_use: { name: bash, input: { command: \"touch made.txt\" } } } ]\n  - content: [ { text: finished } ]\n",
    );
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::ModeChanged(Mode::Ask)).await;
    h.d.request(UiRequest::Submit("make a file".into())).await;
    // In ask mode the bash call needs approval, answered in the chat.
    let env_ = tokio::time::timeout(Duration::from_secs(10), h.prompts.recv())
        .await
        .unwrap()
        .unwrap();
    let id = env_.prompt.requests[0].approval_id.clone();
    env_.reply
        .send(vec![(id, ApprovalAnswer::Approve(ApprovalScope::Once))])
        .unwrap();
    h.finish_turn().await;
    assert!(env.project.join("made.txt").exists());
}

#[tokio::test]
async fn yolo_warns_and_runs_without_prompts() {
    let env = Env::new();
    let script = env.script(
        "responses:\n  - content: [ { tool_use: { name: bash, input: { command: \"touch made.txt\" } } } ]\n  - content: [ { text: finished } ]\n",
    );
    let mut h = Harness::start(&env, &script).await;
    let warned = |e: &AppEvent| {
        matches!(e, AppEvent::Core(CoreEvent::Notify { level: Level::Warning, title, body })
            if title == "yolo mode" && body.as_deref() == Some(harness_policy::approvals::YOLO_WARNING))
    };
    // Shift+Tab (or the palette) reports the change.
    h.d.request(UiRequest::ModeChanged(Mode::Yolo)).await;
    h.until(warned).await;
    // A command that asks in auto-edit runs with no prompt.
    h.d.request(UiRequest::Submit("make a file".into())).await;
    h.finish_turn().await;
    assert!(env.project.join("made.txt").exists());
    assert!(h.prompts.try_recv().is_err(), "no approval prompt in yolo");
    // `/mode yolo` warns too; other modes do not.
    h.d.request(UiRequest::Submit("/mode ask".into())).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Mode(Mode::Ask))))
        .await;
    h.d.request(UiRequest::Submit("/mode yolo".into())).await;
    h.until(warned).await;
    // A session that starts in yolo from harness.ini warns at once.
    let mut h = Harness::with_ini(&env, &script, false, "[general]\nmode = yolo\n").await;
    h.until(warned).await;
}

#[tokio::test]
async fn workflow_commands_prompt_for_missing_inputs_and_gates_are_answered_in_chat() {
    let env = Env::new();
    let fixtures = crate_root().join("../harness-workflow/evals/workflows/fixtures/calc");
    copy_dir(&fixtures, &env.project);
    std::fs::write(
        env.project.join("tests/test_double.sh"),
        ". ./src/calc.sh\n. ./tests/lib.sh\ncheck double \"$(double 4)\" 8\nfinish\n",
    )
    .unwrap();
    let script = crate_root()
        .join("../harness-workflow/evals/workflows/scripts/plan-approve-implement.yaml");
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("/plan".into())).await;
    let entry = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::PromptArgs(_))))
        .await
    {
        AppEvent::Core(CoreEvent::PromptArgs(e)) => e.clone(),
        _ => unreachable!(),
    };
    assert_eq!(entry.id, "workflow.plan");
    assert_eq!(entry.args.len(), 2, "{:?}", entry.args);
    assert!(entry.args.iter().all(|a| a.required && a.hint.is_some()));
    h.d.request(UiRequest::Run(Invocation {
        id: entry.id.clone(),
        args: vec![
            "Add a `double` function to src/calc.sh (double 4 prints 8).".into(),
            "sh run_tests.sh".into(),
        ],
    }))
    .await;
    // The plan gate arrives as an approval prompt for the chat.
    let gate = tokio::time::timeout(Duration::from_secs(30), h.prompts.recv())
        .await
        .expect("gate prompt")
        .unwrap();
    assert!(gate.prompt.requests[0].call_id.starts_with("gate."));
    let id = gate.prompt.requests[0].approval_id.clone();
    gate.reply
        .send(vec![(id, ApprovalAnswer::Approve(ApprovalScope::Once))])
        .unwrap();
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title.contains("succeeded")))
        .await;
    assert!(
        std::fs::read_to_string(env.project.join("src/calc.sh"))
            .unwrap()
            .contains("double()")
    );
    // The workflow's budget reaches the map as a badge on its node.
    h.until(|e| {
        matches!(e, AppEvent::Core(CoreEvent::NodeBadge { node, badge, .. })
        if node.as_str() == "plan-approve-implement" && badge.contains("/$5.00"))
    })
    .await;
    // The plan step's artifact is listed in its node detail and opens.
    let plan = harness_events::schema::NodeId::from("plan-approve-implement/plan");
    h.d.request(UiRequest::Artifacts(plan.clone())).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Artifacts { names, .. }) if names == &["plan".to_owned()]))
        .await;
    h.d.request(UiRequest::OpenArtifact {
        node: plan,
        name: "plan".into(),
    })
    .await;
    let lines = h.panel("Artifact plan").await;
    assert!(lines.len() > 2, "the plan's content: {lines:?}");
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

#[test]
fn parses_tui_flags() {
    let args = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_owned).collect() };
    let o = parse(&args(
        "--resume last --agent reviewer --mode=ask --cwd /tmp",
    ))
    .unwrap();
    assert_eq!(o.resume.as_deref(), Some("last"));
    assert_eq!(o.agent.as_deref(), Some("reviewer"));
    assert_eq!(o.mode, Some(Mode::Ask));
    assert_eq!(o.cwd, Some(PathBuf::from("/tmp")));
    assert!(
        parse(&args("--bogus"))
            .unwrap_err()
            .contains("unknown option")
    );
    assert!(parse(&args("--mode loose")).is_err());
    assert!(
        parse(&args("--agent"))
            .unwrap_err()
            .contains("needs a value")
    );
}

#[tokio::test]
async fn any_turn_checkpoint_can_be_restored() {
    let env = Env::new();
    let script = env.script(
        "responses:\n  - content: [ { tool_use: { name: write, input: { path: a.txt, content: \"a\\n\" } } } ]\n  - content: [ { text: one } ]\n  - content: [ { tool_use: { name: write, input: { path: b.txt, content: \"b\\n\" } } } ]\n  - content: [ { text: two } ]\n",
    );
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("first".into())).await;
    h.finish_turn().await;
    h.d.request(UiRequest::Submit("second".into())).await;
    h.finish_turn().await;
    let turns = h.d.h.as_ref().unwrap().turn_checkpoints.clone();
    assert_eq!(turns.len(), 2);
    assert!(env.project.join("a.txt").exists() && env.project.join("b.txt").exists());
    // Restore to before the second turn: its file goes, the first stays.
    h.d.request(UiRequest::RestoreCheckpoint(turns[1].0.clone()))
        .await;
    h.until(
        |e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "restored"),
    )
    .await;
    assert!(env.project.join("a.txt").exists());
    assert!(!env.project.join("b.txt").exists());
}

#[tokio::test]
async fn hygiene_reports_reach_node_detail_and_temp_files_can_be_removed() {
    let env = Env::new();
    let script = env.script(
        "responses:\n  - content:\n      - tool_use: { name: write, input: { path: src/lib.rs, content: \"pub fn f() {}\\n\" } }\n      - tool_use: { name: write, input: { path: debug.log, content: \"trace\\n\" } }\n  - content: [ { text: done } ]\n",
    );
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("change it".into())).await;
    h.finish_turn().await;
    let (node, report, temp) = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::Hygiene { .. })))
        .await
    {
        AppEvent::Core(CoreEvent::Hygiene {
            node,
            report,
            temp_files,
            ..
        }) => (node.clone(), report.clone(), *temp_files),
        _ => unreachable!(),
    };
    assert!(report[0].starts_with("Hygiene"), "{report:?}");
    assert_eq!(temp, 1, "{report:?}");
    // The problem is reported before the turn counts as done.
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "hygiene"))
        .await;
    // The score is recorded as a metric of the run.
    let metrics =
        h.d.h
            .as_ref()
            .unwrap()
            .run_dir
            .join(harness_hygiene::report::METRICS_FILE);
    assert!(
        std::fs::read_to_string(metrics)
            .unwrap()
            .contains("hygiene.score")
    );
    h.d.request(UiRequest::RemoveTempFiles(node)).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "removed"))
        .await;
    assert!(!env.project.join("debug.log").exists());
    assert!(env.project.join("src/lib.rs").exists());
}

#[tokio::test]
async fn memory_cards_undo_their_commit() {
    let env = Env::new();
    let script = env.script(
        "responses:\n  - content: [ { tool_use: { name: memory, input: { action: write, path: tools/cargo, text: \"Use cargo nextest here.\" } } } ]\n  - content: [ { text: saved } ]\n",
    );
    let mut h = Harness::with(&env, &script, true).await;
    h.d.request(UiRequest::Submit("remember".into())).await;
    h.finish_turn().await;
    let sha = match h
        .until(|e| matches!(e, AppEvent::Run(ev) if matches!(ev.kind, EventKind::Memory { .. })))
        .await
    {
        AppEvent::Run(ev) => match &ev.kind {
            EventKind::Memory { memory_id, .. } => memory_id.clone(),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };
    let checkout = h.d.h.as_ref().unwrap().memory.clone().unwrap();
    let note = checkout.path().join("tools/cargo.md");
    assert!(std::fs::read_to_string(&note).unwrap().contains("nextest"));
    h.d.request(UiRequest::UndoMemory(sha.clone())).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::MemoryUndone(s)) if *s == sha))
        .await;
    let text = std::fs::read_to_string(&note).unwrap_or_default();
    assert!(!text.contains("nextest"), "reverted: {text}");
}

#[tokio::test]
async fn remote_lists_hosts_and_tests_them_with_health_badges() {
    let env = Env::new();
    let script = env.script(TEXT);
    // Port 1 on localhost refuses connections: the test fails fast, offline.
    let ini = "[remote.box]\nhost = 127.0.0.1\nport = 1\nlabels = gpu\n";
    let mut h = Harness::with_ini(&env, &script, false, ini).await;
    h.d.request(UiRequest::Submit("/remote".into())).await;
    let (lines, buttons) = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if title == commands::REMOTE_TITLE))
        .await
    {
        AppEvent::Core(CoreEvent::Panel { lines, buttons, .. }) => (lines.clone(), buttons.clone()),
        _ => unreachable!(),
    };
    assert!(lines.iter().any(|l| l.contains("box")), "{lines:?}");
    assert!(
        lines[0].starts_with("[?"),
        "health unknown before a test: {lines:?}"
    );
    assert_eq!(buttons.len(), 1);
    assert_eq!(buttons[0].id, "remote.test:box");
    h.d.request(UiRequest::PanelAction(buttons[0].id.clone()))
        .await;
    let text = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::PanelText { lines, .. }) if lines.iter().any(|l| l.contains("connection:"))))
        .await
    {
        AppEvent::Core(CoreEvent::PanelText { lines, .. }) => lines.clone(),
        _ => unreachable!(),
    };
    assert!(
        text.iter().any(|l| l.contains("connection: FAILED")),
        "{text:?}"
    );
    assert!(text.iter().any(|l| l.contains("helper:")), "{text:?}");
    assert!(
        text[0].starts_with("[down"),
        "the listing shows the new health: {text:?}"
    );
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::HostBadge { host, badge, .. }) if host == "box" && badge == "down"))
        .await;
}

#[tokio::test]
async fn memory_dream_consolidates_in_the_background() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::with(&env, &script, true).await;
    h.d.request(UiRequest::Submit("/memory dream".into())).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "consolidating memory"))
        .await;
    let body = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "memory consolidated" || title == "dream failed"))
        .await
    {
        AppEvent::Core(CoreEvent::Notify { title, body, .. }) => {
            assert_eq!(title, "memory consolidated", "{body:?}");
            body.clone()
        }
        _ => unreachable!(),
    };
    assert!(body.is_some());
    let log = env.root.join("data/memory/.harness/dream-log.jsonl");
    assert!(log.exists(), "the pass is logged");
}

#[tokio::test]
async fn a_scheduled_pass_runs_when_due_after_activity() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::with(&env, &script, true).await;
    h.d.request(UiRequest::Submit("do something".into())).await;
    h.finish_turn().await;
    // Activity and no pass yet: due ([memory] dream = daily).
    commands::dream(&mut h.d, false).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "memory consolidated"))
        .await;
    // Right after a pass nothing is due.
    let before = h.seen.len();
    commands::dream(&mut h.d, false).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(ev) = h.rx.try_recv() {
        h.seen.push(ev);
    }
    assert!(
        !h.seen[before..]
            .iter()
            .any(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "memory consolidated")),
        "a second pass ran"
    );
}

#[test]
fn lessons_use_the_configured_dream_model() {
    let cfg = harness_config::ini::load_str(
        "[provider.anthropic]\napi_key = test-key-not-used\n[memory]\ndream_model = haiku\n",
        None,
    )
    .unwrap();
    let (provider, model) = crate::dream::lesson_model(&cfg, None).expect("a provider");
    assert_eq!(provider.name(), "anthropic");
    assert_eq!(model, "claude-haiku-5-5");
    // Without a key the pass runs without lessons.
    let cfg = harness_config::ini::load_str("", None).unwrap();
    assert!(crate::dream::lesson_model(&cfg, None).is_none());
}

#[tokio::test]
async fn the_memory_toggle_keeps_notes_and_removes_the_tool_for_the_session() {
    let env = Env::new();
    let script = env.script(
        "responses:\n  - content: [ { tool_use: { name: memory, input: { action: write, path: tools/cargo, text: \"Use cargo nextest here.\" } } } ]\n  - content: [ { text: saved } ]\n  - content: [ { text: next } ]\n",
    );
    let mut h = Harness::with(&env, &script, true).await;
    h.d.request(UiRequest::Submit("remember".into())).await;
    h.finish_turn().await;
    h.d.request(UiRequest::Submit("/memory".into())).await;
    let data = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::Memory(_))))
        .await
    {
        AppEvent::Core(CoreEvent::Memory(d)) => d.clone(),
        _ => unreachable!(),
    };
    assert!(data.enabled);
    assert!(
        data.files.iter().any(|f| f.path == "tools/cargo.md"),
        "{data:?}"
    );
    assert!(!data.recent.is_empty(), "the write is a recent commit");
    // Off: the session continues without the memory tool; notes stay.
    h.d.request(UiRequest::Memory(
        harness_tui::app::MemoryRequest::SetEnabled(false),
    ))
    .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "memory off for this session"))
        .await;
    assert!(h.d.h.as_ref().unwrap().memory.is_none());
    assert!(
        !h.d.h
            .as_ref()
            .unwrap()
            .tools
            .iter()
            .any(|t| t.name == "memory")
    );
    let note = env.root.join("data/memory/tools/cargo.md");
    assert!(
        std::fs::read_to_string(&note).unwrap().contains("nextest"),
        "notes are kept"
    );
    // The conversation continues in the same session.
    let session = h.d.core.as_ref().unwrap().session_id().to_owned();
    h.d.request(UiRequest::Memory(
        harness_tui::app::MemoryRequest::SetEnabled(true),
    ))
    .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "memory on for this session"))
        .await;
    assert_eq!(h.d.core.as_ref().unwrap().session_id(), session);
    assert!(
        h.d.h
            .as_ref()
            .unwrap()
            .tools
            .iter()
            .any(|t| t.name == "memory")
    );
    // Search goes through the drive's index.
    h.d.request(UiRequest::Memory(harness_tui::app::MemoryRequest::Search(
        "nextest".into(),
    )))
    .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::MemorySearch(rows)) if rows.iter().any(|r| r.path.contains("tools/cargo"))))
        .await;
}

/// No API keys resolve, so the catalog never fetches live lists.
const NO_KEYS: &str = "[provider.anthropic]\napi_key_env = HARNESS_T135_UNSET_ANTHROPIC\n\
     [provider.openai]\napi_key_env = HARNESS_T135_UNSET_OPENAI\n\
     [provider.openrouter]\napi_key_env = HARNESS_T135_UNSET_OPENROUTER\n";

fn is_picker(e: &AppEvent) -> bool {
    matches!(e, AppEvent::Core(CoreEvent::ModelPicker { .. }))
}

#[tokio::test]
async fn model_without_argument_and_the_palette_entry_open_the_picker() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::with_ini(&env, &script, false, NO_KEYS).await;
    let entries = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::PaletteEntries(_))))
        .await
    {
        AppEvent::Core(CoreEvent::PaletteEntries(v)) => v.clone(),
        _ => unreachable!(),
    };
    let entry = entries
        .iter()
        .find(|e| e.id == harness_tui::model_picker::PALETTE_ID)
        .expect("palette entry");
    assert!(entry.args.is_empty(), "opens without an argument prompt");

    let started = std::time::Instant::now();
    h.d.request(UiRequest::Submit("/model".into())).await;
    let took = started.elapsed();
    let current = h.d.core.as_ref().unwrap().model.reference.clone();
    match h.until(is_picker).await {
        AppEvent::Core(CoreEvent::ModelPicker {
            catalog,
            current: c,
        }) => {
            assert_eq!(c.as_deref(), Some(current.as_str()));
            assert!(!catalog.entries().is_empty(), "built-in models listed");
            assert!(!catalog.refreshing(), "nothing to fetch without keys");
        }
        _ => unreachable!(),
    }
    // Best of five opens: a slow implementation is slow every time, while
    // a scheduler hiccup on a shared CI runner only hits some of them.
    let mut took = took;
    for _ in 1..5 {
        if took.as_millis() < 50 {
            break;
        }
        h.seen.clear();
        let started = std::time::Instant::now();
        h.d.request(UiRequest::Submit("/model".into())).await;
        took = took.min(started.elapsed());
        h.until(is_picker).await;
    }
    assert!(
        took.as_millis() < 50,
        "opened in {took:?} (best of up to 5)"
    );
    assert!(
        !h.seen
            .iter()
            .any(|e| matches!(e, AppEvent::Core(CoreEvent::Panel { .. }))),
        "no panel"
    );

    h.seen.clear();
    h.d.request(UiRequest::Run(Invocation {
        id: harness_tui::model_picker::PALETTE_ID.into(),
        args: vec![],
    }))
    .await;
    h.until(is_picker).await;
    // The `/model` command entry with an empty argument opens it too.
    h.seen.clear();
    h.d.request(UiRequest::Run(Invocation {
        id: "cmd.model".into(),
        args: vec![String::new()],
    }))
    .await;
    h.until(is_picker).await;
}

#[tokio::test]
async fn model_with_a_name_switches_directly_and_the_picker_choice_switches() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::with_ini(&env, &script, false, NO_KEYS).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Session { .. })))
        .await;
    h.seen.clear();
    h.d.request(UiRequest::Submit("/model haiku".into())).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Session { .. })))
        .await;
    assert!(!h.seen.iter().any(is_picker), "{:?}", h.seen);
    assert_eq!(h.d.opts.model.as_deref(), Some("haiku"));

    h.seen.clear();
    h.d.request(UiRequest::SetModel("anthropic/claude-opus-5-5".into()))
        .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Session { .. })))
        .await;
    assert!(!h.seen.iter().any(is_picker));
    assert_eq!(h.d.opts.model.as_deref(), Some("anthropic/claude-opus-5-5"));
}

/// Accepts only one key.
struct OneKey(&'static str);

#[async_trait::async_trait]
impl crate::login::Verifier for OneKey {
    async fn verify(
        &self,
        provider: &str,
        key: &harness_config::secrets::Secret,
        _base_url: Option<&str>,
    ) -> Result<harness_providers::login::KeyCheck, harness_providers::provider::ProviderError>
    {
        if key.expose() == self.0 {
            Ok(harness_providers::login::KeyCheck {
                provider: provider.into(),
                url: "http://mock.local/key".into(),
                detail: None,
            })
        } else {
            Err(harness_providers::provider::ProviderError::Auth(
                "invalid key".into(),
            ))
        }
    }
}

/// Every file under `dir`, read as bytes.
fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if std::fs::read(&p)
                .map(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
                .unwrap_or(false)
            {
                out.push(p);
            }
        }
    }
    out
}

/// [`files_containing`] once the run store's buffered writer has flushed:
/// polls until a file contains `needle` or 10 s pass.
async fn files_containing_eventually(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let found = files_containing(dir, needle);
        if !found.is_empty() || tokio::time::Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// What the `/login` modal sends for an inline key.
fn submitted(provider: &str, key: &str) -> harness_tui::login::LoginSubmission {
    use harness_tui::login::{LoginAction, LoginModal, LoginOutcome};
    let mut m = LoginModal::new(Some(provider)).unwrap();
    m.activate(LoginAction::Option(0));
    m.paste(key);
    match m.activate(LoginAction::Primary) {
        LoginOutcome::Submit(s) => s,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn login_opens_the_modal_and_saves_a_checked_key_without_leaking_it() {
    // Shapes no built-in redaction pattern knows, so only registering them
    // masks them.
    const GOOD: &str = "tui-login-good-key-0123456789";
    const BAD: &str = "tui-login-bad-key-9876543210";
    for k in [GOOD, BAD] {
        assert_eq!(Redactor::default().redact(k), k, "precondition");
    }
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::with_ini(&env, &script, false, NO_KEYS).await;
    let ini = env.root.join("cfg").join("harness.ini");
    h.d.login_path = ini.clone();
    h.d.login_verifier = Arc::new(OneKey(GOOD));

    // `/login` is a palette entry and a slash command, with or without
    // the provider.
    let entries = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::PaletteEntries(_))))
        .await
    {
        AppEvent::Core(CoreEvent::PaletteEntries(v)) => v.clone(),
        _ => unreachable!(),
    };
    assert!(entries.iter().any(|e| e.id == "cmd.login"), "palette entry");
    h.d.request(UiRequest::Run(Invocation {
        id: "cmd.login".into(),
        args: vec![String::new()],
    }))
    .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Login { provider: None })))
        .await;
    h.d.request(UiRequest::Submit("/login openrouter".into()))
        .await;
    h.until(|e| {
        matches!(e, AppEvent::Core(CoreEvent::Login { provider: Some(p) }) if p == "openrouter")
    })
    .await;

    // A rejected key: an error for the modal, nothing saved.
    h.d.request(UiRequest::Login(submitted("openrouter", BAD)))
        .await;
    match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::LoginResult(_))))
        .await
    {
        AppEvent::Core(CoreEvent::LoginResult(Err(e))) => {
            assert!(e.contains("OpenRouter rejected the key"), "{e}");
            assert!(e.contains("Nothing was saved"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!ini.exists());
    h.seen.clear();

    // An accepted key: saved with mode 0600 and used by the provider.
    h.d.request(UiRequest::Login(submitted("openrouter", GOOD)))
        .await;
    match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::LoginResult(_))))
        .await
    {
        AppEvent::Core(CoreEvent::LoginResult(Ok(m))) => {
            assert!(m.contains("OpenRouter accepted the key"), "{m}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        std::fs::read_to_string(&ini)
            .unwrap()
            .contains(&format!("api_key = {GOOD}"))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&ini).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let resolved = h.d.cfg.providers["openrouter"].resolve_api_key().unwrap();
    assert_eq!(resolved.unwrap().expose(), GOOD);

    // Both keys are now redacted, and a new session uses that redactor:
    // a prompt quoting them is stored masked.
    for k in [GOOD, BAD] {
        assert!(!h.d.redactor.redact(k).contains(k), "{k} redacted");
    }
    h.d.request(UiRequest::Submit("/new".into())).await;
    h.d.request(UiRequest::Submit(format!("keys: {GOOD} and {BAD}")))
        .await;
    h.finish_turn().await;

    // Events reach disk through a buffered writer; wait for the flush.
    assert!(
        !files_containing_eventually(&env.root.join("data"), "keys: [REDACTED] and [REDACTED]")
            .await
            .is_empty(),
        "the prompt is stored, masked"
    );
    // No UI event and no stored event or file carries either key.
    let seen = format!("{:?}", h.seen);
    let events = format!("{:?}", h.d.events.lock().unwrap());
    for k in [GOOD, BAD] {
        assert!(!seen.contains(k), "{k} in a UI event");
        assert!(!events.contains(k), "{k} in a run event");
        assert_eq!(
            files_containing(&env.root.join("data"), k),
            Vec::<PathBuf>::new(),
            "{k} stored"
        );
    }
}

/// Records the links it is asked to open instead of starting a browser.
#[derive(Default)]
struct RecordingBrowser(std::sync::Mutex<Vec<String>>);

impl harness_providers::chatgpt::Browser for RecordingBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(url.to_owned());
        Ok(())
    }
}

#[tokio::test]
async fn usage_limit_offers_manage_usage_as_the_primary_button() {
    // TASK-133 AC6: hitting the ChatGPT plan usage limit stops the turn and
    // shows the error with *Manage usage* as its primary (first, focused)
    // button, operable by keyboard and mouse, with no fallback call.
    use harness_tui::app::{App, AppConfig};
    use harness_tui::interaction::testing::{buffer_text, click, find_text, key};
    use harness_tui::panel::PanelButton;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let url = harness_providers::chatgpt::MANAGE_USAGE_URL;
    let env = Env::new();
    let script = env.script(
        "responses:\n  - error: { kind: usage_limit, message: \"You have reached your plan's usage limit.\" }\n  - content: [ { text: \"must not be used\" } ]\n",
    );
    let mut h = Harness::start(&env, &script).await;
    let browser = Arc::new(RecordingBrowser::default());
    h.d.browser = browser.clone();
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Session { .. })))
        .await;
    h.d.request(UiRequest::Submit("hi".into())).await;
    h.finish_turn().await;

    let (lines, buttons) = match h
        .until(|e| {
            matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if title == commands::ERROR_TITLE)
        })
        .await
    {
        AppEvent::Core(CoreEvent::Panel { lines, buttons, .. }) => (lines.clone(), buttons.clone()),
        _ => unreachable!(),
    };
    assert_eq!(
        buttons,
        vec![PanelButton::new(commands::ERROR_ACTION, "Manage usage")],
        "Manage usage is the first (primary) button"
    );
    assert!(lines.iter().any(|l| l.contains("usage limit")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains(url)), "{lines:?}");
    // No silent fallback: the second scripted reply never streamed.
    assert!(
        !h.seen.iter().any(|e| matches!(
            e,
            AppEvent::Run(ev) if matches!(&ev.kind, EventKind::Delta { text, .. } if text.contains("must not"))
        )),
        "a second model call was made"
    );

    // The real app renders the panel; the button is focused, so Enter runs it.
    let (tx, mut requests) = mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), tx);
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    app.handle(AppEvent::Core(CoreEvent::Panel {
        title: commands::ERROR_TITLE.into(),
        lines,
        buttons,
    }));
    term.draw(|f| app.render(f)).unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(text.contains("[ Manage usage ]"), "{text}");
    app.handle(AppEvent::Terminal(key("enter")));
    let req = requests.try_recv().expect("Enter activates Manage usage");
    assert_eq!(req, UiRequest::PanelAction(commands::ERROR_ACTION.into()));
    h.d.request(req).await;
    assert_eq!(*browser.0.lock().unwrap(), vec![url.to_owned()]);
    h.until(|e| {
        matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "opened in the browser")
    })
    .await;

    // Mouse: clicking the button does the same.
    term.draw(|f| app.render(f)).unwrap();
    let (x, y) = find_text(term.backend().buffer(), "[ Manage usage ]").unwrap();
    app.handle(AppEvent::Terminal(click(x + 2, y)));
    let req = requests.try_recv().expect("a click activates Manage usage");
    assert_eq!(req, UiRequest::PanelAction(commands::ERROR_ACTION.into()));
    h.d.request(req).await;
    assert_eq!(browser.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn sessions_are_named_renamed_listed_and_resumed_by_picker_and_name() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("Please fix the header parser".into()))
        .await;
    h.finish_turn().await;
    let first = h.d.core.as_ref().unwrap().session().clone();
    assert_eq!(first.name, "Fix header parser");
    assert_eq!(first.description, "Please fix the header parser");
    assert_eq!(first.messages, 2);
    assert!(first.last_message_at.is_some());

    // /rename shows the name without an argument and saves a new one.
    h.d.request(UiRequest::Submit("/rename".into())).await;
    h.until(|e| {
        matches!(e, AppEvent::Core(CoreEvent::Notify { body: Some(b), .. }) if b.starts_with("Fix header parser"))
    })
    .await;
    h.d.request(UiRequest::Submit("/rename Parser   work".into()))
        .await;
    assert_eq!(h.d.core.as_ref().unwrap().session().name, "Parser work");
    // The chat confirms the new name.
    h.until(|e| {
        matches!(e, AppEvent::Core(CoreEvent::Notify { level: Level::Success, title, .. }) if title == "Session renamed to Parser work")
    })
    .await;
    let store = harness_agent::session::SessionStore::new(h.d.data.sessions());
    assert_eq!(store.load(&first.id).unwrap().name, "Parser work");

    // A second session, then the picker lists both, newest first.
    h.d.request(UiRequest::Submit("/new".into())).await;
    h.d.request(UiRequest::Submit("write the docs".into()))
        .await;
    h.finish_turn().await;
    let second = h.d.core.as_ref().unwrap().session().clone();
    assert_ne!(second.id, first.id);
    h.seen.clear();
    h.d.request(UiRequest::Submit("/resume".into())).await;
    let rows = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::SessionPicker(_))))
        .await
    {
        AppEvent::Core(CoreEvent::SessionPicker(rows)) => rows.clone(),
        _ => unreachable!(),
    };
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Write docs", "Parser work"]);
    assert!(rows[0].current && !rows[1].current);
    assert!(rows[1].when.ends_with("today") || rows[1].when.starts_with("yesterday"));
    assert_eq!(rows[1].messages, "2 msgs");
    assert_eq!(rows[1].description, "Please fix the header parser");

    // The palette offers the other session.
    let entries = commands::palette_entries(&h.d.registry, h.d.core.as_ref().unwrap());
    let sessions: Vec<&harness_tui::palette::PaletteEntry> = entries
        .iter()
        .filter(|e| e.kind == harness_tui::palette::EntryKind::Session)
        .collect();
    assert_eq!(sessions.len(), 1);
    assert!(
        sessions[0].title.starts_with("Resume Parser work · "),
        "{}",
        sessions[0].title
    );

    // Choosing it in the picker resumes it.
    h.d.request(UiRequest::Resume(first.id.clone())).await;
    assert_eq!(h.d.core.as_ref().unwrap().session_id(), first.id);
    assert_eq!(h.d.core.as_ref().unwrap().conversation.messages.len(), 2);
    // So does the palette entry, and `/resume <name>`.
    h.d.request(UiRequest::Run(Invocation {
        id: format!("{}{}", harness_tui::palette::ids::SESSION_PREFIX, second.id),
        args: Vec::new(),
    }))
    .await;
    assert_eq!(h.d.core.as_ref().unwrap().session_id(), second.id);
    h.d.request(UiRequest::Submit("/resume parser work".into()))
        .await;
    assert_eq!(h.d.core.as_ref().unwrap().session_id(), first.id);
    // An unknown name keeps the open session.
    h.seen.clear();
    h.d.request(UiRequest::Submit("/resume no such thing".into()))
        .await;
    h.until(|e| {
        matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "cannot resume")
    })
    .await;
    assert_eq!(h.d.core.as_ref().unwrap().session_id(), first.id);
}

/// The `session-summary` events of the run log in `dir`.
fn summary_events(dir: &Path) -> Vec<harness_events::schema::RunEvent> {
    let replay = harness_events::store::read_run(dir).unwrap();
    let nodes: Vec<_> = replay
        .events
        .iter()
        .filter(|e| {
            matches!(&e.kind, EventKind::NodeStarted { name, .. } if name == crate::sessions::SUMMARY_NODE)
        })
        .filter_map(|e| e.node_id.clone())
        .collect();
    replay
        .events
        .into_iter()
        .filter(|e| e.node_id.as_ref().is_some_and(|n| nodes.contains(n)))
        .collect()
}

#[tokio::test]
async fn the_summary_runs_after_the_turn_and_is_recorded_with_its_usage() {
    let env = Env::new();
    let script = env.script(
        "responses:\n  - content: [ { text: done } ]\n  - delay_ms: 200\n    content: [ { text: \"name: Colon parsing\\ndescription: Fixed colons in the header parser\" } ]\n    usage: { input_tokens: 40, output_tokens: 8 }\n",
    );
    let ini = "[general]\nsession_summary_model = cheap\n";
    let mut h = Harness::with_ini(&env, &script, false, ini).await;
    h.d.request(UiRequest::Submit("fix the header parser".into()))
        .await;
    h.finish_turn().await;
    // The turn ends without waiting for the summary; the summary lands in
    // the session file when its call finishes.
    let id = h.d.core.as_ref().unwrap().session_id().to_owned();
    let store = harness_agent::session::SessionStore::new(h.d.data.sessions());
    h.until(|e| {
        matches!(e, AppEvent::Run(ev) if matches!(&ev.kind, EventKind::NodeFinished { .. })
            && ev.parent_node_id.is_some())
    })
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let saved = loop {
        let m = store.load(&id).unwrap();
        if m.name == "Colon parsing" {
            break m;
        }
        assert!(tokio::time::Instant::now() < deadline, "summary not saved");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(saved.description, "Fixed colons in the header parser");
    assert_eq!(saved.tokens, 48, "the summary call's tokens count");
    let dir = h.d.core.as_ref().unwrap().recorder.dir().to_path_buf();
    h.d.finish_core().await;
    let events = summary_events(&dir);
    assert!(
        events.iter().all(|e| e.parent_node_id.is_some()),
        "a child node, so resume never mistakes it for the conversation"
    );
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::ModelCallFinished { usage, .. } if usage.input == 40 && usage.output == 8
    )));
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::NodeFinished {
            status: harness_events::schema::NodeStatus::Succeeded,
            ..
        }
    )));
    // The session on disk keeps the summary after the core closes.
    assert_eq!(store.load(&id).unwrap().name, "Colon parsing");
}

#[tokio::test]
async fn a_slow_summary_never_delays_turns_or_closing_and_cancelled_turns_skip_it() {
    let env = Env::new();
    let script = env.script(
        "responses:\n  - content: [ { text: done } ]\n  - delay_ms: 60000\n    content: [ { text: \"name: Too late\\ndescription: never\" } ]\n",
    );
    let ini = "[general]\nsession_summary_model = cheap\n";
    let mut h = Harness::with_ini(&env, &script, false, ini).await;
    h.d.request(UiRequest::Submit("fix the header parser".into()))
        .await;
    let t = h.d.turn.take().unwrap();
    let (core, outcome) = tokio::time::timeout(Duration::from_secs(20), t)
        .await
        .expect("the turn ends without waiting for the 60 s summary")
        .unwrap();
    h.d.turn_done(core, outcome).await;
    h.until(|e| {
        matches!(e, AppEvent::Run(ev) if matches!(&ev.kind, EventKind::ModelCallStarted { .. })
            && ev.parent_node_id.is_some())
    })
    .await;
    let id = h.d.core.as_ref().unwrap().session_id().to_owned();
    let dir = h.d.core.as_ref().unwrap().recorder.dir().to_path_buf();
    // Closing the session (/new, /resume, quit) cancels the summary.
    tokio::time::timeout(Duration::from_secs(20), h.d.finish_core())
        .await
        .expect("closing does not wait for the summary");
    let events = summary_events(&dir);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::NodeFinished {
            status: harness_events::schema::NodeStatus::Cancelled,
            ..
        }
    )));
    let store = harness_agent::session::SessionStore::new(h.d.data.sessions());
    assert_eq!(
        store.load(&id).unwrap().description,
        "fix the header parser"
    );

    // A cancelled turn makes no summary call.
    let env = Env::new();
    let script = env.script(
        "responses:\n  - delay_ms: 30000\n    content: [ { text: late } ]\n  - content: [ { text: \"name: X\\ndescription: Y\" } ]\n",
    );
    let mut h = Harness::with_ini(&env, &script, false, ini).await;
    h.d.request(UiRequest::Submit("go".into())).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.d.request(UiRequest::Interrupt).await;
    let t = h.d.turn.take().unwrap();
    let (core, outcome) = tokio::time::timeout(Duration::from_secs(10), t)
        .await
        .expect("cancelled promptly")
        .unwrap();
    assert_eq!(
        outcome.stop,
        harness_agent::agent_loop::StopReason::Cancelled
    );
    h.d.turn_done(core, outcome).await;
    let dir = h.d.core.as_ref().unwrap().recorder.dir().to_path_buf();
    h.d.finish_core().await;
    assert!(summary_events(&dir).is_empty(), "no summary after a cancel");
}

#[tokio::test]
async fn login_mcp_authorizes_from_slash_palette_and_panel_buttons() {
    // TASK-146.3: `/login mcp` lists remote servers with an authorize
    // button each (Enter and click), `/login mcp <name>` and the palette
    // entry start the OAuth flow, and tokens never reach the UI.
    use harness_mcp::auth::mock::{MockBrowser, MockConfig, MockOAuthServer};
    use harness_tui::app::{App, AppConfig};
    use harness_tui::interaction::testing::{buffer_text, click, find_text, key};
    use harness_tui::panel::PanelButton;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mock = MockOAuthServer::start(MockConfig::default()).await;
    let env = Env::new();
    let script = env.script(TEXT);
    let ini_text = format!(
        "[general]\ndata_dir = {}\n\n[mcp.docs]\nurl = {}\n",
        env.root.join("data").display(),
        mock.mcp_url()
    );
    let mut h = Harness::with_ini(&env, &script, false, &ini_text).await;
    let ini = env.root.join("cfg").join("harness.ini");
    std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
    std::fs::write(&ini, &ini_text).unwrap();
    h.d.login_path = ini.clone();
    let browser = Arc::new(MockBrowser::new());
    h.d.browser = browser.clone();
    let mut all = String::new();

    // The palette has an entry per remote server.
    let entries = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::PaletteEntries(_))))
        .await
    {
        AppEvent::Core(CoreEvent::PaletteEntries(v)) => v.clone(),
        _ => unreachable!(),
    };
    let entry = entries
        .iter()
        .find(|e| e.id == "mcp.login.docs")
        .expect("palette entry");
    assert_eq!(entry.title, "Authorize MCP server: docs");

    // `/login mcp` opens the panel with one button per remote server.
    h.d.request(UiRequest::Submit("/login mcp".into())).await;
    let (lines, buttons) = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if title == "MCP authorization"))
        .await
    {
        AppEvent::Core(CoreEvent::Panel { lines, buttons, .. }) => (lines.clone(), buttons.clone()),
        _ => unreachable!(),
    };
    assert_eq!(
        buttons,
        vec![PanelButton::new("mcp.login:docs", "authorize docs")]
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("docs") && l.contains("auth = none")),
        "{lines:?}"
    );

    // Keyboard: Enter on the focused button asks the core to authorize.
    let (tx, mut requests) = mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), tx);
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    app.handle(AppEvent::Core(CoreEvent::Panel {
        title: "MCP authorization".into(),
        lines,
        buttons,
    }));
    term.draw(|f| app.render(f)).unwrap();
    assert!(buffer_text(term.backend().buffer()).contains("[ authorize docs ]"));
    app.handle(AppEvent::Terminal(key("enter")));
    let req = requests.try_recv().expect("Enter activates the button");
    assert_eq!(req, UiRequest::PanelAction("mcp.login:docs".into()));
    h.d.request(req).await;
    // The sign-in modal gets the link; the local browser opens on a
    // desktop.
    match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::SignIn(p)) if p.id == "mcp.docs"))
        .await
    {
        AppEvent::Core(CoreEvent::SignIn(p)) => {
            assert_eq!(p.title, "Authorize MCP server docs");
            assert!(p.url.contains("code_challenge="), "{}", p.url);
            assert_eq!(p.no_browser, None);
            assert_eq!(p.forward, None);
        }
        _ => unreachable!(),
    }
    match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title.contains("docs authorized") || title.contains("failed")))
        .await
    {
        AppEvent::Core(CoreEvent::Notify { level, body, .. }) => {
            assert_eq!(*level, Level::Success, "{body:?}");
        }
        _ => unreachable!(),
    }
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::PanelText { lines, .. }) if lines.iter().any(|l| l.contains("authorized") && !l.contains("not authorized"))))
        .await;
    assert!(
        std::fs::read_to_string(&ini)
            .unwrap()
            .contains("auth = oauth")
    );
    assert_eq!(browser.opened().len(), 1);

    // Mouse: clicking the button does the same.
    term.draw(|f| app.render(f)).unwrap();
    let (x, y) = find_text(term.backend().buffer(), "[ authorize docs ]").unwrap();
    app.handle(AppEvent::Terminal(click(x + 2, y)));
    let req = requests.try_recv().expect("a click activates the button");
    assert_eq!(req, UiRequest::PanelAction("mcp.login:docs".into()));
    all.push_str(&format!("{:?}", h.seen));
    h.seen.clear();
    h.d.request(req).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "MCP server docs authorized"))
        .await;

    // The palette entry and `/login mcp docs` run the flow too.
    all.push_str(&format!("{:?}", h.seen));
    h.seen.clear();
    h.d.request(UiRequest::Run(Invocation {
        id: "mcp.login.docs".into(),
        args: Vec::new(),
    }))
    .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "MCP server docs authorized"))
        .await;
    all.push_str(&format!("{:?}", h.seen));
    h.seen.clear();
    h.d.request(UiRequest::Submit("/login mcp docs".into()))
        .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "MCP server docs authorized"))
        .await;
    assert_eq!(browser.opened().len(), 4);

    // An unknown server is an error toast; tokens never reach the UI.
    h.d.request(UiRequest::Submit("/login mcp nope".into()))
        .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { body: Some(b), .. }) if b.contains("`nope` is not a remote MCP server")))
        .await;
    let cfg = harness_config::ini::load_file(&ini).unwrap();
    all.push_str(&format!("{:?}", h.seen));
    for t in crate::wiring::mcp_token_store(&cfg).secrets() {
        assert!(!all.contains(&t), "a token reached the UI");
    }
}

mod mcp;
mod signin;
mod update;

/// `/jobs`: background bash jobs are listed with their state, their output
/// opens and a running one stops (mouse and keyboard on the panel buttons);
/// ending the session stops the rest.
#[tokio::test]
async fn jobs_panel_lists_opens_and_stops_background_jobs() {
    use harness_tui::interaction::testing::{buffer_text, click, find_text, key};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let env = Env::new();
    let bg = |file: &str| {
        format!(
            "  - content: [ {{ tool_use: {{ name: bash, input: {{ command: \"echo serving; sleep 60 & echo $! > {file}; wait\", run_in_background: true }} }} }} ]\n  - content: [ {{ text: started }} ]\n"
        )
    };
    let script = env.script(&format!("responses:\n{}{}", bg("pid1"), bg("pid2")));
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::ModeChanged(Mode::Auto)).await;
    let wait_pid = |file: &str| {
        let path = env.project.join(file);
        async move {
            for _ in 0..200 {
                if let Ok(s) = std::fs::read_to_string(&path)
                    && s.ends_with('\n')
                {
                    return s.trim().parse::<u32>().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            panic!("{} never written", path.display());
        }
    };
    let dead = |pid: u32| async move {
        for _ in 0..100 {
            if harness_tools::conformance::local_process_gone(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("process {pid} survived");
    };

    h.d.request(UiRequest::Submit("start the server".into()))
        .await;
    h.finish_turn().await;
    let pid1 = wait_pid("pid1").await;
    h.until(|e| matches!(e, AppEvent::Run(ev) if matches!(&ev.kind, EventKind::BashJob { state, .. } if state == "running")))
        .await;

    h.seen.clear();
    h.d.request(UiRequest::Submit("/jobs".into())).await;
    let (lines, buttons) = match h
        .until(
            |e| matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if title == jobs::TITLE),
        )
        .await
    {
        AppEvent::Core(CoreEvent::Panel { lines, buttons, .. }) => (lines.clone(), buttons.clone()),
        _ => unreachable!(),
    };
    assert_eq!(lines[0], "1 running · 0 ended", "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("j1") && l.contains("running") && l.contains("echo serving")),
        "{lines:?}"
    );
    assert!(lines.iter().any(|l| l.contains("log ")), "{lines:?}");
    let ids: Vec<&str> = buttons.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["jobs.output:j1", "jobs.stop:j1", "jobs.refresh"]);

    // Output opens in its own panel. The pid file proves the shell printed
    // "serving", but the job's pump task appends it to the log on its own
    // schedule, so refresh the panel (as a user would) until it shows up.
    let mut out = Vec::new();
    for _ in 0..100 {
        h.seen.retain(
            |e| !matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if title == "Job j1"),
        );
        h.d.request(UiRequest::PanelAction("jobs.output:j1".into()))
            .await;
        out = h.panel("Job j1").await;
        if out.iter().any(|l| l == "serving") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(out.iter().any(|l| l == "serving"), "{out:?}");

    // The real app renders the list; Right + Enter and a click both ask to stop.
    let (tx, mut requests) = mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), tx);
    let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
    app.handle(AppEvent::Core(CoreEvent::Panel {
        title: jobs::TITLE.into(),
        lines,
        buttons,
    }));
    term.draw(|f| app.render(f)).unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(text.contains("[ stop j1 ]"), "{text}");
    app.handle(AppEvent::Terminal(key("right")));
    app.handle(AppEvent::Terminal(key("enter")));
    let stop = UiRequest::PanelAction("jobs.stop:j1".into());
    assert_eq!(requests.try_recv().expect("Enter on stop"), stop);
    term.draw(|f| app.render(f)).unwrap();
    let (x, y) = find_text(term.backend().buffer(), "[ stop j1 ]").unwrap();
    app.handle(AppEvent::Terminal(click(x + 2, y)));
    assert_eq!(requests.try_recv().expect("a click on stop"), stop);

    h.seen.clear();
    h.d.request(stop).await;
    h.until(
        |e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "stopped j1"),
    )
    .await;
    dead(pid1).await;
    let after = h.panel(jobs::TITLE).await;
    assert_eq!(after[0], "0 running · 1 ended", "{after:?}");
    assert!(
        after
            .iter()
            .any(|l| l.starts_with("j1") && l.contains("stopped")),
        "{after:?}"
    );

    // Ending the session (quit, /new, /resume) stops the jobs still running.
    h.d.request(UiRequest::Submit("again".into())).await;
    h.finish_turn().await;
    let pid2 = wait_pid("pid2").await;
    h.d.finish_core().await;
    dead(pid2).await;
}

/// A subagent's background jobs live in the session's manager: the
/// subagent hears about the job that ended while it ran, the job still
/// running when it ended is listed and stopped in `/jobs`, and its end is
/// reported to the top-level agent.
#[tokio::test]
async fn subagent_jobs_are_reported_listed_and_stopped_with_the_session() {
    let env = Env::new();
    let agents = env.project.join(".harness/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("coder.yaml"),
        "name: coder\nprompt_text: You lead.\ntools: [bash]\nsubagents: [worker]\n",
    )
    .unwrap();
    std::fs::write(
        agents.join("worker.yaml"),
        "name: worker\ndescription: Works\nprompt_text: You work.\ntools: [bash]\n",
    )
    .unwrap();
    let mut cfg = harness_config::ini::load_str("", None).unwrap();
    cfg.general.data_dir = env.root.join("data");
    let data = harness_config::paths::DataDir::from_config(&cfg)
        .open()
        .unwrap();
    let snap = harness_config::trust::snapshot_project(&env.project)
        .unwrap()
        .unwrap();
    harness_config::trust::TrustStore::from_data_dir(&data)
        .trust(&env.project, &snap)
        .unwrap();
    let script = env.script(
        r#"responses:
  - content: [ { tool_use: { name: spawn_agent, input: { agent: worker, brief: "build it" } } } ]
  - content: [ { tool_use: { name: bash, input: { command: "sleep 0.3; echo child-out; exit 7", run_in_background: true } } } ]
  - content: [ { tool_use: { name: bash, input: { command: "sleep 1" } } } ]
  - content: [ { tool_use: { name: bash, input: { command: "sleep 60 & echo $! > pid; wait", run_in_background: true } } } ]
  - content: [ { text: "worker done" } ]
  - content: [ { text: "lead done" } ]
  - content: [ { text: "ok" } ]
"#,
    );
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::ModeChanged(Mode::Auto)).await;
    h.d.request(UiRequest::Submit("build".into())).await;
    h.finish_turn().await;
    let pid_file = env.project.join("pid");
    let mut pid = None;
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(&pid_file)
            && s.ends_with('\n')
        {
            pid = Some(s.trim().parse::<u32>().unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let pid = pid.expect("the worker's job wrote its pid");

    // Requests as recorded: (child?, messages).
    let requests = |seen: &[AppEvent]| -> Vec<(bool, String)> {
        seen.iter()
            .filter_map(|e| match e {
                AppEvent::Run(ev) => match &ev.kind {
                    EventKind::ModelCallStarted {
                        request: harness_events::schema::Payload::Json(v),
                        ..
                    } => Some((ev.parent_node_id.is_some(), v["messages"].to_string())),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    };
    let notice = "Notice from harness: background job";
    // 2 requests of the top-level agent, 4 of the worker.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while requests(&h.seen).len() < 6 {
        match tokio::time::timeout_at(deadline, h.rx.recv()).await {
            Ok(Some(ev)) => h.seen.push(ev),
            _ => panic!("requests missing: {:?}", requests(&h.seen)),
        }
    }
    let reqs = requests(&h.seen);
    let child: Vec<&String> = reqs.iter().filter(|r| r.0).map(|r| &r.1).collect();
    assert_eq!(child.len(), 4, "{reqs:?}");
    assert!(
        child[2].contains(&format!("{notice} j1 ended (exit code 7)")),
        "the worker hears about its own job: {}",
        child[2]
    );
    assert!(child[2].contains("child-out"), "{}", child[2]);
    assert!(!child[3].contains("j2 ended"), "{}", child[3]);
    let top: Vec<&String> = reqs.iter().filter(|r| !r.0).map(|r| &r.1).collect();
    assert!(
        top.iter().all(|m| !m.contains(notice)),
        "the worker's job is not reported twice: {top:?}"
    );

    // /jobs lists the worker's jobs (one manager per session) and stops
    // the one still running.
    h.seen.clear();
    h.d.request(UiRequest::Submit("/jobs".into())).await;
    let (lines, buttons) = match h
        .until(
            |e| matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if title == jobs::TITLE),
        )
        .await
    {
        AppEvent::Core(CoreEvent::Panel { lines, buttons, .. }) => (lines.clone(), buttons.clone()),
        _ => unreachable!(),
    };
    assert_eq!(lines[0], "1 running · 1 ended", "{lines:?}");
    let ids: Vec<&str> = buttons.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "jobs.output:j1",
            "jobs.output:j2",
            "jobs.stop:j2",
            "jobs.refresh"
        ]
    );
    h.d.request(UiRequest::PanelAction("jobs.stop:j2".into()))
        .await;
    h.until(
        |e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "stopped j2"),
    )
    .await;
    for _ in 0..100 {
        if harness_tools::conformance::local_process_gone(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(harness_tools::conformance::local_process_gone(pid));
    h.until(|e| matches!(e, AppEvent::Run(ev) if matches!(&ev.kind, EventKind::BashJob { job_id, state, .. } if job_id == "j2" && state == "stopped")))
        .await;

    // The worker is gone, so the top-level agent hears about j2.
    h.seen.clear();
    h.d.request(UiRequest::Submit("next".into())).await;
    h.finish_turn().await;
    h.until(
        |e| matches!(e, AppEvent::Run(ev) if matches!(ev.kind, EventKind::ModelCallStarted { .. })),
    )
    .await;
    let reqs = requests(&h.seen);
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    let last = &reqs.last().unwrap().1;
    assert!(
        last.contains(&format!("{notice} j2 ended (stopped)")),
        "{last}"
    );
    assert!(!last.contains(&format!("{notice} j1")), "{last}");
    h.d.finish_core().await;
}

/// The `thinking` of every top-level request seen so far, oldest first.
fn top_level_thinking(seen: &[AppEvent]) -> Vec<serde_json::Value> {
    seen.iter()
        .filter_map(|e| match e {
            AppEvent::Run(ev) if ev.parent_node_id.is_none() => match &ev.kind {
                EventKind::ModelCallStarted {
                    request: harness_events::schema::Payload::Json(v),
                    ..
                } => Some(v["thinking"].clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

const TWO_TEXTS: &str = "responses:\n  - content: [ { text: \"one\" } ]\n  - content: [ { text: \"two\" } ]\n  - content: [ { text: \"three\" } ]\n";

/// TASK-163 AC1, AC2, AC5: `/effort low` reaches the next request, shows
/// in the status line and survives resume; `/effort default` restores the
/// agent's (none for the coder) and the session file forgets the override.
#[tokio::test]
async fn effort_sets_the_next_request_shows_in_the_status_and_survives_resume() {
    let env = Env::new();
    let script = env.script(TWO_TEXTS);
    let mut h = Harness::start(&env, &script).await;
    // No effort yet: nothing in the status line, no thinking on the wire.
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Effort(None))))
        .await;
    h.d.request(UiRequest::Submit("first".into())).await;
    h.finish_turn().await;
    h.until(|e| matches!(e, AppEvent::Run(ev) if matches!(ev.kind, EventKind::ModelCallFinished { .. })))
        .await;
    assert_eq!(top_level_thinking(&h.seen), vec![serde_json::Value::Null]);

    h.d.request(UiRequest::Submit("/effort low".into())).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Effort(Some(s))) if s == "effort low"))
        .await;
    h.until(
        |e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "effort low"),
    )
    .await;
    let id = h.d.core.as_ref().unwrap().session_id().to_owned();
    let store = harness_agent::session::SessionStore::new(h.d.data.sessions());
    assert_eq!(
        store.load(&id).unwrap().effort,
        Some(harness_config::agents::Effort::Low)
    );
    h.seen.clear();
    h.d.request(UiRequest::Submit("second".into())).await;
    h.finish_turn().await;
    h.until(|e| matches!(e, AppEvent::Run(ev) if matches!(ev.kind, EventKind::ModelCallFinished { .. })))
        .await;
    assert_eq!(
        top_level_thinking(&h.seen),
        vec![serde_json::json!({"effort": "low"})]
    );

    // Resume (a new core from the session file) restores it.
    h.seen.clear();
    h.d.request(UiRequest::Submit("/new".into())).await;
    assert_eq!(
        h.d.core.as_ref().unwrap().effort().session,
        None,
        "/new resets"
    );
    h.d.request(UiRequest::Resume(id.clone())).await;
    assert_eq!(h.d.core.as_ref().unwrap().session_id(), id);
    assert_eq!(
        h.d.core.as_ref().unwrap().effort().effective(),
        Some(harness_config::agents::Effort::Low)
    );
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Effort(Some(s))) if s == "effort low"))
        .await;

    // `default` clears the override.
    h.d.request(UiRequest::Submit("/effort default".into()))
        .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "effort model default"))
        .await;
    assert_eq!(store.load(&id).unwrap().effort, None);
    h.seen.clear();
    h.d.request(UiRequest::Submit("third".into())).await;
    h.finish_turn().await;
    h.until(|e| matches!(e, AppEvent::Run(ev) if matches!(ev.kind, EventKind::ModelCallFinished { .. })))
        .await;
    assert_eq!(top_level_thinking(&h.seen), vec![serde_json::Value::Null]);
}

/// TASK-163 AC2: `default` restores the agent YAML's effort (the
/// researcher's `low`), and an override wins over it.
#[tokio::test]
async fn effort_default_restores_the_agent_yaml_effort() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("/researcher".into())).await;
    let core = h.d.core.as_ref().unwrap();
    assert_eq!(core.agent.name, "researcher");
    assert_eq!(
        core.effort().effective(),
        Some(harness_config::agents::Effort::Low)
    );
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Effort(Some(s))) if s == "effort low"))
        .await;
    h.d.request(UiRequest::Submit("/effort high".into())).await;
    let state = h.d.core.as_ref().unwrap().effort();
    assert_eq!(
        state.effective(),
        Some(harness_config::agents::Effort::High)
    );
    assert_eq!(state.agent, Some(harness_config::agents::Effort::Low));
    h.d.request(UiRequest::Submit("/effort default".into()))
        .await;
    let state = h.d.core.as_ref().unwrap().effort();
    assert_eq!(state.session, None);
    assert_eq!(state.effective(), Some(harness_config::agents::Effort::Low));
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, body: Some(b), .. }) if title == "effort low" && b.contains("YAML")))
        .await;
}

/// TASK-163 AC4: on a model that does not reason `/effort` says so, and
/// the request carries the effort only for providers to drop.
#[tokio::test]
async fn effort_on_a_model_without_reasoning_warns() {
    let env = Env::new();
    let script = env.script(&format!(
        "capabilities: {{ tools: true, thinking: false, context_window: 1000 }}\n{TEXT}"
    ));
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("/effort".into())).await;
    let lines = h.panel(effort::TITLE).await;
    assert!(
        lines.iter().any(|l| l.contains("does not reason")),
        "{lines:?}"
    );
    h.d.request(UiRequest::Submit("/effort high".into())).await;
    h.until(|e| {
        matches!(e, AppEvent::Core(CoreEvent::Notify { level: Level::Warning, title, body: Some(b) })
            if title == "effort high: not sent" && b.contains("does not reason"))
    })
    .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Effort(Some(s))) if s == "no reasoning"))
        .await;
    // An unknown value is an error, never a silent default.
    h.d.request(UiRequest::Submit("/effort extreme".into()))
        .await;
    assert_eq!(
        h.d.core.as_ref().unwrap().effort().session,
        Some(harness_config::agents::Effort::High)
    );
}

/// TASK-163 AC1 and the UI rule: the bare `/effort` picker shows the
/// active effort ("model default"), and its buttons work by keyboard
/// (Right to focus, Enter) and by mouse.
#[tokio::test]
async fn effort_picker_works_with_keyboard_and_mouse() {
    use harness_tui::app::{App, AppConfig};
    use harness_tui::interaction::testing::{buffer_text, click, find_text, key};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::start(&env, &script).await;
    h.d.request(UiRequest::Submit("/effort".into())).await;
    let (lines, buttons) = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if title == effort::TITLE))
        .await
    {
        AppEvent::Core(CoreEvent::Panel { lines, buttons, .. }) => (lines.clone(), buttons.clone()),
        _ => unreachable!(),
    };
    assert!(
        lines.iter().any(|l| l.starts_with("effort  model default")),
        "{lines:?}"
    );
    assert!(lines.iter().any(|l| l.contains("fake/")), "{lines:?}");
    // The fake model's levels are unknown, so every level is offered.
    let ids: Vec<String> = buttons.iter().map(|b| b.id.clone()).collect();
    let want: Vec<String> = effort::CHOICES
        .iter()
        .map(|c| format!("effort.set:{c}"))
        .collect();
    assert_eq!(ids, want);

    let (tx, mut requests) = mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), tx);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    app.handle(AppEvent::Core(CoreEvent::Panel {
        title: effort::TITLE.into(),
        lines,
        buttons,
    }));
    term.draw(|f| app.render(f)).unwrap();
    let text = buffer_text(term.backend().buffer());
    for c in effort::CHOICES {
        let b = format!("[ {c} ]");
        assert!(text.contains(&b), "{b}: {text}");
    }
    // Keyboard: the first button is focused; Right twice moves to `low`.
    app.handle(AppEvent::Terminal(key("right")));
    app.handle(AppEvent::Terminal(key("right")));
    app.handle(AppEvent::Terminal(key("enter")));
    let req = requests
        .try_recv()
        .expect("Enter activates the focused button");
    assert_eq!(req, UiRequest::PanelAction("effort.set:low".into()));
    h.d.request(req).await;
    assert_eq!(
        h.d.core.as_ref().unwrap().effort().session,
        Some(harness_config::agents::Effort::Low)
    );
    // The open panel is refreshed with the new effort.
    h.until(|e| {
        matches!(e, AppEvent::Core(CoreEvent::PanelText { title, lines })
            if title == effort::TITLE && lines.iter().any(|l| l.starts_with("effort  low")))
    })
    .await;

    // Mouse: clicking `[ high ]` does the same.
    term.draw(|f| app.render(f)).unwrap();
    let (x, y) = find_text(term.backend().buffer(), "[ high ]").unwrap();
    app.handle(AppEvent::Terminal(click(x + 2, y)));
    let req = requests.try_recv().expect("a click activates the button");
    assert_eq!(req, UiRequest::PanelAction("effort.set:high".into()));
    h.d.request(req).await;
    assert_eq!(
        h.d.core.as_ref().unwrap().effort().session,
        Some(harness_config::agents::Effort::High)
    );
    // Esc closes the picker.
    app.handle(AppEvent::Terminal(key("esc")));
    assert!(app.panel().is_none());
}

/// TASK-163 AC6: `/effort` is in `/help`, the palette and slash completion
/// with its choices.
#[tokio::test]
async fn effort_is_in_help_palette_and_completion() {
    let env = Env::new();
    let script = env.script(TEXT);
    let mut h = Harness::start(&env, &script).await;
    let help = h.d.registry.help(None);
    assert!(
        help.iter()
            .any(|l| l.contains("/effort") && l.contains("reasoning effort")),
        "{help:?}"
    );
    let one = h.d.registry.help(Some("effort"));
    assert!(
        one.iter()
            .any(|l| l.contains("default, minimal, low, medium, high, xhigh, max, ultra")),
        "{one:?}"
    );
    let entries = commands::palette_entries(&h.d.registry, h.d.core.as_ref().unwrap());
    let e = entries
        .iter()
        .find(|e| e.id == "cmd.effort")
        .expect("palette entry");
    assert_eq!(e.args.len(), 1);
    // The palette runs it with its argument.
    h.d.request(UiRequest::Run(Invocation {
        id: "cmd.effort".into(),
        args: vec!["medium".into()],
    }))
    .await;
    assert_eq!(
        h.d.core.as_ref().unwrap().effort().session,
        Some(harness_config::agents::Effort::Medium)
    );
    let slash = commands::slash_commands(&h.d.registry);
    let s = slash.iter().find(|s| s.name == "effort").unwrap();
    assert_eq!(s.choices, effort::CHOICES);
    assert_eq!(
        s.args.as_deref(),
        Some("[default|minimal|low|medium|high|xhigh|max|ultra]")
    );
    // Busy: the change is queued for after the turn.
    h.d.request(UiRequest::Submit("go".into())).await;
    assert!(h.d.busy());
    h.d.request(UiRequest::Submit("/effort high".into())).await;
    assert_eq!(h.d.queue.back().map(String::as_str), Some("/effort high"));
    h.finish_turn().await;
    assert_eq!(
        h.d.core.as_ref().unwrap().effort().session,
        Some(harness_config::agents::Effort::High)
    );
}
