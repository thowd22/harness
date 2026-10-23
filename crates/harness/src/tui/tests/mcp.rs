//! TASK-146.4: MCP in the TUI. The `/mcp` panel (status, transport, tool
//! count, auth; restart, disable for the session, logs; mouse and
//! keyboard), prompts as namespaced slash commands and palette entries,
//! resources attached as untrusted content, and the failed-server
//! indicator. Servers are the bundled test server over stdio.

use harness_tui::app::{App, AppConfig};
use harness_tui::interaction::testing::{buffer_text, click, find_text, key};
use harness_tui::panel::PanelButton;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

/// harness.ini with test servers: `t` (eager), `lazy`, `off` (disabled)
/// and `bad` (eager, cannot start).
fn ini(env: &Env) -> String {
    let bin = harness_mcp::test_server::binary_path();
    format!(
        "[general]\ndata_dir = {data}\n\n\
         [mcp.t]\ncommand = {bin}\nstartup = eager\n\n\
         [mcp.lazy]\ncommand = {bin}\n\n\
         [mcp.off]\ncommand = {bin}\nenabled = false\n\n\
         [mcp.bad]\ncommand = {missing}\nstartup = eager\n",
        data = env.root.join("data").display(),
        bin = bin.display(),
        missing = env.root.join("no-such-mcp-server").display(),
    )
}

async fn start(env: &Env) -> Harness {
    let script = env.script(&format!(
        "{TEXT}{}",
        &TEXT["responses:\n".len()..].repeat(4)
    ));
    Harness::with_ini(env, &script, false, &ini(env)).await
}

impl Harness {
    /// The last `Panel` event with `title`: lines and buttons.
    async fn panel_full(&mut self, title: &str) -> (Vec<String>, Vec<PanelButton>) {
        let t = title.to_owned();
        let i = {
            self.until(
                |e| matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if *title == t),
            )
            .await;
            self.seen
                .iter()
                .rposition(
                    |e| matches!(e, AppEvent::Core(CoreEvent::Panel { title, .. }) if *title == t),
                )
                .unwrap()
        };
        match self.seen.remove(i) {
            AppEvent::Core(CoreEvent::Panel { lines, buttons, .. }) => (lines, buttons),
            _ => unreachable!(),
        }
    }

    /// Waits until the watcher has the eager servers settled: `t`
    /// listed (its prompts reached completion) and `bad` failed.
    async fn settled(&mut self) {
        self.until(|e| matches!(e, AppEvent::Core(CoreEvent::McpStatus { failed }) if failed == &["bad".to_owned()]))
            .await;
        self.until(|e| matches!(e, AppEvent::Core(CoreEvent::SlashCommands(v)) if v.iter().any(|c| c.name == "t:review")))
            .await;
    }

    /// Sends the text of the background prompt run, as the driver's loop
    /// does.
    async fn mcp_turn(&mut self) {
        let text = tokio::time::timeout(Duration::from_secs(20), self.d.mcp.next_turn())
            .await
            .expect("the MCP prompt produced a turn")
            .expect("turn channel open");
        self.d.prompt(text).await;
    }

    /// The text of the user messages recorded so far.
    async fn user_messages(&mut self) -> Vec<String> {
        self.finish_turn().await;
        let events = self.d.events.lock().unwrap().clone();
        events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::UserMessage { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
}

fn line<'a>(lines: &'a [String], server: &str) -> &'a str {
    lines
        .iter()
        .find(|l| l.starts_with(&format!("{server} ")))
        .unwrap_or_else(|| panic!("no line for {server}: {lines:#?}"))
}

#[tokio::test]
async fn mcp_panel_shows_servers_and_its_buttons_work_by_keyboard_and_mouse() {
    let env = Env::new();
    let mut h = start(&env).await;
    h.settled().await;

    h.d.request(UiRequest::Submit("/mcp".into())).await;
    let (lines, buttons) = h.panel_full("MCP servers").await;
    let t = line(&lines, "t");
    assert!(t.contains("connected (MCP"), "{t}");
    assert!(t.contains("stdio"), "{t}");
    assert!(t.contains(" 12 "), "tool count: {t}");
    assert!(t.contains("no credentials"), "{t}");
    assert!(line(&lines, "lazy").contains("idle (starts on first use)"));
    assert!(line(&lines, "off").contains("disabled (harness.ini)"));
    assert!(line(&lines, "bad").contains("failed"), "{lines:#?}");
    let reason = lines
        .iter()
        .find(|l| l.starts_with("  reason: ") && l.contains("could not start"))
        .unwrap_or_else(|| panic!("failure reason shown: {lines:#?}"));
    assert!(reason.contains("check `command`"), "{reason}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("prompts: /t:review, /t:greet · resources: 2")),
        "{lines:#?}"
    );
    let ids: Vec<&str> = buttons.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "mcp.restart:bad",
            "mcp.disable:bad",
            "mcp.logs:bad",
            "mcp.restart:lazy",
            "mcp.disable:lazy",
            "mcp.logs:lazy",
            "mcp.logs:off",
            "mcp.restart:t",
            "mcp.disable:t",
            "mcp.logs:t",
        ]
    );

    // The panel in the UI: Tab reaches "disable t", Enter activates it.
    let (tx, mut requests) = mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), tx);
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    app.handle(AppEvent::Core(CoreEvent::Panel {
        title: "MCP servers".into(),
        lines,
        buttons,
    }));
    term.draw(|f| app.render(f)).unwrap();
    assert!(buffer_text(term.backend().buffer()).contains("[ disable t ]"));
    // The first button has focus; eight Tabs reach the ninth.
    for _ in 0..8 {
        app.handle(AppEvent::Terminal(key("tab")));
        term.draw(|f| app.render(f)).unwrap();
    }
    app.handle(AppEvent::Terminal(key("enter")));
    let req = requests
        .try_recv()
        .expect("Enter activates the focused button");
    assert_eq!(req, UiRequest::PanelAction("mcp.disable:t".into()));
    let server =
        h.d.core
            .as_ref()
            .unwrap()
            .mcp_servers()
            .get("t")
            .unwrap()
            .clone();
    assert!(server.pgid().await.is_some(), "t runs");
    h.seen.clear();
    h.d.request(req).await;
    let (lines, buttons) = h.panel_full("MCP servers").await;
    assert!(
        line(&lines, "t").contains("disabled (this session)"),
        "{lines:#?}"
    );
    assert!(buttons.contains(&PanelButton::new("mcp.enable:t", "enable t")));
    // Stopping runs in the background and says when it is done.
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "MCP server t disabled for this session"))
        .await;
    assert_eq!(server.pgid().await, None, "the server was stopped");
    // Its prompts leave the palette and completion.
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::SlashCommands(v)) if !v.iter().any(|c| c.name.starts_with("t:"))))
        .await;

    // Mouse: clicking "[ enable t ]" asks to enable it.
    app.handle(AppEvent::Core(CoreEvent::Panel {
        title: "MCP servers".into(),
        lines,
        buttons,
    }));
    term.draw(|f| app.render(f)).unwrap();
    let (x, y) = find_text(term.backend().buffer(), "[ enable t ]").unwrap();
    while requests.try_recv().is_ok() {}
    app.handle(AppEvent::Terminal(click(x + 2, y)));
    let req = requests.try_recv().expect("a click activates the button");
    assert_eq!(req, UiRequest::PanelAction("mcp.enable:t".into()));
    h.seen.clear();
    h.d.request(req).await;
    let (lines, _) = h.panel_full("MCP servers").await;
    assert!(line(&lines, "t").contains("idle"), "{lines:#?}");

    // Restart starts it again; the watcher updates the open panel.
    h.seen.clear();
    h.d.request(UiRequest::PanelAction("mcp.restart:t".into()))
        .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "MCP server t connected"))
        .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::PanelText { title, lines }) if title == "MCP servers" && line(lines, "t").contains("connected")))
        .await;

    // Logs: status, last error and stderr; back returns to the list.
    h.d.request(UiRequest::PanelAction("mcp.logs:bad".into()))
        .await;
    let (lines, buttons) = h.panel_full("MCP logs: bad").await;
    assert!(lines[0].starts_with("status: failed"), "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("last error: could not start"))
    );
    assert_eq!(buttons[0], PanelButton::new("mcp.back", "back"));
    h.d.request(UiRequest::PanelAction("mcp.back".into())).await;
    h.panel_full("MCP servers").await;
    h.d.finish_core().await;
}

#[tokio::test]
async fn failed_servers_show_a_status_line_indicator_that_opens_mcp() {
    let env = Env::new();
    let mut h = start(&env).await;
    h.settled().await;
    let (tx, mut requests) = mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), tx);
    let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    app.handle(AppEvent::Core(CoreEvent::McpStatus {
        failed: vec!["bad".into()],
    }));
    term.draw(|f| app.render(f)).unwrap();
    let (x, y) = find_text(term.backend().buffer(), "⚠ mcp bad").expect("indicator");
    app.handle(AppEvent::Terminal(click(x + 1, y)));
    assert_eq!(
        requests.try_recv().unwrap(),
        UiRequest::Submit("/mcp".into())
    );
    // Disabling the failed server for the session clears it.
    h.seen.clear();
    h.d.request(UiRequest::PanelAction("mcp.disable:bad".into()))
        .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::McpStatus { failed }) if failed.is_empty()))
        .await;
    h.d.finish_core().await;
}

#[tokio::test]
async fn mcp_prompts_are_namespaced_slash_commands_and_palette_entries() {
    let env = Env::new();
    let mut h = start(&env).await;
    h.settled().await;
    let slash = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::SlashCommands(v)) if v.iter().any(|c| c.name == "t:review")))
        .await
    {
        AppEvent::Core(CoreEvent::SlashCommands(v)) => v.clone(),
        _ => unreachable!(),
    };
    let review = slash.iter().find(|c| c.name == "t:review").unwrap();
    assert_eq!(review.args.as_deref(), Some("<pr> [focus...]"));
    assert_eq!(review.description, "Reviews a pull request. (MCP t)");
    assert!(slash.iter().any(|c| c.name == "help"), "built-ins stay");
    let palette = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::PaletteEntries(v)) if v.iter().any(|p| p.id == "mcp.prompt.t:review")))
        .await
    {
        AppEvent::Core(CoreEvent::PaletteEntries(v)) => v.clone(),
        _ => unreachable!(),
    };
    let entry = palette
        .iter()
        .find(|p| p.id == "mcp.prompt.t:review")
        .unwrap();
    assert_eq!(entry.title, "/t:review");
    assert_eq!(entry.kind, harness_tui::palette::EntryKind::Mcp);
    assert_eq!(entry.args.len(), 2);
    assert!(entry.args[0].required);
    assert_eq!(entry.args[0].hint.as_deref(), Some("Pull request number"));
    assert!(palette.iter().any(|p| p.id == "cmd.help"), "built-ins stay");

    // Typed, with the last argument taking the rest of the line.
    h.d.request(UiRequest::Submit("/t:review 42 the tests".into()))
        .await;
    h.mcp_turn().await;
    let sent = h.user_messages().await;
    let text = sent.last().expect("a turn started");
    assert!(
        text.starts_with("Review pull request #42, focusing on the tests."),
        "{text}"
    );
    // The embedded resource is labeled as untrusted content.
    assert!(
        text.contains("<web_content url=\"test://diff\" server=\"t\" trust=\"untrusted\" origin=\"mcp server\">\ndiff of #42"),
        "{text}"
    );

    // A missing required argument is asked for in the palette.
    h.seen.clear();
    h.d.request(UiRequest::Submit("/t:review".into())).await;
    match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::PromptArgs(_))))
        .await
    {
        AppEvent::Core(CoreEvent::PromptArgs(e)) => {
            assert_eq!(e.id, "mcp.prompt.t:review");
            assert_eq!(e.args[0].name, "pr");
        }
        _ => unreachable!(),
    }
    // The palette's answers run it (an empty optional is left out).
    h.d.request(UiRequest::Run(Invocation {
        id: "mcp.prompt.t:review".into(),
        args: vec!["7".into(), String::new()],
    }))
    .await;
    h.mcp_turn().await;
    let sent = h.user_messages().await;
    assert!(sent.last().unwrap().starts_with("Review pull request #7."));

    // A server that has not started yet: running its prompt starts it.
    h.d.request(UiRequest::Submit("/lazy:greet".into())).await;
    h.mcp_turn().await;
    let sent = h.user_messages().await;
    assert_eq!(sent.last().unwrap(), "Say hello.");

    // Unknown prompts name the ones that exist.
    h.seen.clear();
    h.d.request(UiRequest::Submit("/t:nope".into())).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { body: Some(b), .. }) if b.contains("no prompt `nope`") && b.contains("/t:review, /t:greet")))
        .await;
    h.d.finish_core().await;
}

#[tokio::test]
async fn mcp_resources_attach_to_the_next_message_as_untrusted_content() {
    let env = Env::new();
    let mut h = start(&env).await;
    h.settled().await;
    let palette = match h
        .until(|e| matches!(e, AppEvent::Core(CoreEvent::PaletteEntries(v)) if v.iter().any(|p| p.id.starts_with("mcp.resource."))))
        .await
    {
        AppEvent::Core(CoreEvent::PaletteEntries(v)) => v.clone(),
        _ => unreachable!(),
    };
    let readme = palette
        .iter()
        .find(|p| p.id == "mcp.resource.t/test://readme")
        .unwrap();
    assert_eq!(readme.title, "Attach MCP resource: t/readme");
    assert_eq!(
        readme.description,
        "Test readme · test://readme · text/markdown"
    );
    // In the palette, searching by server and name finds it; Enter
    // chooses it.
    let (tx, mut requests) = mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), tx);
    let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    app.handle(AppEvent::Core(CoreEvent::PaletteEntries(palette.clone())));
    app.handle(AppEvent::Terminal(key("ctrl+p")));
    for ev in harness_tui::interaction::testing::type_text("t/readme") {
        app.handle(AppEvent::Terminal(ev));
    }
    term.draw(|f| app.render(f)).unwrap();
    assert!(buffer_text(term.backend().buffer()).contains("Attach MCP resource: t/readme"));
    app.handle(AppEvent::Terminal(key("enter")));
    let chosen = requests.try_recv().expect("Enter runs the entry");
    assert_eq!(
        chosen,
        UiRequest::Run(Invocation {
            id: readme.id.clone(),
            args: Vec::new(),
        })
    );

    h.seen.clear();
    h.d.request(chosen).await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "attached t/readme"))
        .await;
    h.d.request(UiRequest::Submit("/mcp".into())).await;
    let (lines, buttons) = h.panel_full("MCP servers").await;
    assert!(lines.contains(&"Attached to your next message: t/readme".to_owned()));
    assert!(buttons.contains(&PanelButton::new("mcp.detach", "detach resources")));

    h.d.request(UiRequest::Submit("summarize it".into())).await;
    let sent = h.user_messages().await;
    let text = sent.last().unwrap();
    assert!(
        text.starts_with("<web_content url=\"test://readme\" server=\"t\" trust=\"untrusted\" origin=\"mcp server\">\n# Readme\n"),
        "{text}"
    );
    // Content cannot close its own marker.
    assert!(
        text.contains("&lt;/web_content> closing attempt\n</web_content>\n\nsummarize it"),
        "{text}"
    );
    // Only the next message carries it.
    h.d.request(UiRequest::Submit("again".into())).await;
    assert_eq!(h.user_messages().await.last().unwrap(), "again");

    // Detach drops attachments before they are sent.
    h.d.request(UiRequest::Run(Invocation {
        id: "mcp.resource.t/test://logo".into(),
        args: Vec::new(),
    }))
    .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "attached t/logo"))
        .await;
    h.d.request(UiRequest::PanelAction("mcp.detach".into()))
        .await;
    h.d.request(UiRequest::Submit("plain".into())).await;
    assert_eq!(h.user_messages().await.last().unwrap(), "plain");
    h.d.finish_core().await;
}

#[tokio::test]
async fn attached_resources_are_redacted_with_the_servers_own_credentials() {
    // The server's `env.*` value is not one of harness's own secrets, so
    // only the server's redactor knows it. (The variable's name avoids the
    // built-in `*TOKEN=` / `*SECRET=` patterns, which would mask it anyway.)
    let env = Env::new();
    let bin = harness_mcp::test_server::binary_path();
    let secret = "mcp-env-credential-7f3a91";
    let ini = format!(
        "[mcp.cred]\ncommand = {bin}\nstartup = eager\nenv.MCP_VALUE = {secret}\n",
        bin = bin.display(),
    );
    let mut h = Harness::with_ini(&env, &env.script(TEXT), false, &ini).await;
    h.d.request(UiRequest::Run(Invocation {
        id: "mcp.resource.cred/test://env/MCP_VALUE".into(),
        args: Vec::new(),
    }))
    .await;
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title.starts_with("attached cred/")))
        .await;
    h.d.request(UiRequest::Submit("check it".into())).await;
    let sent = h.user_messages().await;
    let text = sent.last().unwrap();
    assert!(text.contains("MCP_VALUE=[REDACTED]"), "{text}");
    assert!(!text.contains(secret), "the credential leaked: {text}");
    assert!(text.contains("trust=\"untrusted\""), "{text}");
    for e in &h.seen {
        assert!(
            !format!("{e:?}").contains(secret),
            "leaked to the UI: {e:?}"
        );
    }
    h.d.finish_core().await;
}

#[tokio::test]
async fn interrupt_and_panel_buttons_work_while_a_slow_mcp_prompt_starts_its_server() {
    // `slow` takes a minute to start. Running its prompt and disabling it
    // must not hold up the driver: Interrupt still stops the running turn.
    let env = Env::new();
    let bin = harness_mcp::test_server::binary_path();
    let ini = format!(
        "[mcp.slow]\ncommand = {bin}\nargs = --startup-delay-ms, 60000\nstartup_timeout = 120s\n",
        bin = bin.display(),
    );
    let script = env.script("responses:\n  - delay_ms: 30000\n    content: [ { text: late } ]\n");
    let mut h = Harness::with_ini(&env, &script, false, &ini).await;
    let server =
        h.d.core
            .as_ref()
            .unwrap()
            .mcp_servers()
            .get("slow")
            .unwrap()
            .clone();
    h.d.request(UiRequest::Submit("go".into())).await;
    assert!(h.d.busy(), "a turn runs");

    let limit = Duration::from_secs(10);
    tokio::time::timeout(limit, h.d.request(UiRequest::Submit("/slow:greet".into())))
        .await
        .expect("running a prompt does not wait for its server");
    h.until(|e| matches!(e, AppEvent::Core(CoreEvent::Notify { title, .. }) if title == "starting MCP server slow"))
        .await;
    assert_eq!(server.status().await, harness_mcp::Status::Starting);
    // Disable waits for the start in the background, not in the driver.
    tokio::time::timeout(
        limit,
        h.d.request(UiRequest::PanelAction("mcp.disable:slow".into())),
    )
    .await
    .expect("disable does not wait for the start");

    tokio::time::timeout(limit, h.d.request(UiRequest::Interrupt))
        .await
        .expect("interrupt is handled");
    let t = h.d.turn.take().unwrap();
    let (core, outcome) = tokio::time::timeout(limit, t)
        .await
        .expect("cancelled promptly")
        .unwrap();
    assert_eq!(
        outcome.stop,
        harness_agent::agent_loop::StopReason::Cancelled
    );
    h.d.turn_done(core, outcome).await;
    // Ending the session aborts the pending start; nothing lingers.
    tokio::time::timeout(Duration::from_secs(30), h.d.finish_core())
        .await
        .expect("the session ends without waiting for the server");
}
