//! MCP in the TUI: the `/mcp` panel, server prompts as namespaced slash
//! commands, attachable resources and the status-line indicator.
//!
//! - `/mcp` opens a panel listing every `[mcp.<name>]` server with its
//!   status (connected, starting, failed with the reason, disabled, idle),
//!   transport, tool count and auth state, and buttons to restart it,
//!   disable or enable it for the session and view its logs. Every button
//!   is clickable and reachable with Tab and Enter, like every panel button.
//! - Prompts of running servers become slash commands `/<server>:<prompt>`
//!   (with their arguments; the last one takes the rest of the line) and
//!   palette entries. `/<server>:<prompt>` also works for a server that has
//!   not started yet: it starts it and looks the prompt up. A missing
//!   required argument is asked for in the palette. The prompt's text
//!   becomes the next user turn; embedded resources in it are labeled as
//!   untrusted content.
//! - Resources of running servers are palette entries "Attach MCP
//!   resource: <server>/<name>" (fuzzy search covers server and name).
//!   Choosing one reads it and attaches it to the next message, wrapped in
//!   an untrusted-content marker (`origin="mcp server"`).
//! - A background watcher follows every server's status changes and
//!   refreshes the panel text, the palette and slash completion, and the
//!   failed-server indicator ([`CoreEvent::McpStatus`]).
//!
//! Lazy servers stay lazy: nothing here starts a server except `/mcp`'s
//! restart button and running one of its prompts.
//!
//! Server work (prompts, resource reads, restart, disable and enable) runs
//! in background tasks of [`McpUi`], so the driver keeps handling Interrupt
//! and Quit while a server starts or answers slowly. Attached resource text
//! is redacted with the server's own credentials before it is labeled.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use harness_agent::commands::{ArgSpec as CmdArg, CommandError, CommandKind, CommandSpec};
use harness_config::ini::{Config, McpServer, McpTransport};
use harness_mcp::auth::McpTokenStore;
use harness_mcp::prompts::{Prompt, PromptPart, Resource};
use harness_mcp::{ServerHandle, Servers, Status};
use harness_policy::provenance::{Labeler, Source};
use harness_tui::app::{AppEvent, CoreEvent};
use harness_tui::input::SlashCommand;
use harness_tui::notify::Level;
use harness_tui::palette::{ArgSpec, EntryKind, PaletteEntry};
use harness_tui::panel::PanelButton;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::Driver;

/// Title of the `/mcp` panel.
pub const TITLE: &str = "MCP servers";
/// Title prefix of a server's log panel.
pub const LOGS_TITLE: &str = "MCP logs: ";
/// Panel button: restart (or start) a server (`mcp.restart:<name>`).
pub const RESTART: &str = "mcp.restart:";
/// Panel button: disable a server for the session.
pub const DISABLE: &str = "mcp.disable:";
/// Panel button: enable a server disabled for the session.
pub const ENABLE: &str = "mcp.enable:";
/// Panel button: a server's log.
pub const LOGS: &str = "mcp.logs:";
/// Panel button: back from a log to the server list.
pub const BACK: &str = "mcp.back";
/// Panel button: drop the attached resources.
pub const DETACH: &str = "mcp.detach";
/// Palette id prefix of prompts (`mcp.prompt.<server>:<prompt>`).
pub const PROMPT_PREFIX: &str = "mcp.prompt.";
/// Palette id prefix of resources (`mcp.resource.<server>/<uri>`).
pub const RESOURCE_PREFIX: &str = "mcp.resource.";
/// Most characters of one attached resource or prompt sent to the model.
pub const MAX_CONTENT_CHARS: usize = 100_000;
/// Most characters of server-provided text in titles and descriptions.
const MAX_LABEL_CHARS: usize = 120;

/// What a running server offers, read when it connects or changes.
#[derive(Debug, Clone, Default)]
struct Listing {
    /// The server's status generation when this was read.
    generation: u64,
    tools: Option<usize>,
    prompts: Vec<Prompt>,
    resources: Vec<Resource>,
}

#[derive(Default)]
struct Shared {
    listings: BTreeMap<String, Listing>,
    /// The session's palette entries and slash commands without MCP's.
    base_palette: Vec<PaletteEntry>,
    base_slash: Vec<SlashCommand>,
    /// Resources attached to the next message.
    attachments: Vec<Attachment>,
    /// Servers the user disabled for the session: the wanted state, which
    /// background tasks apply to the servers (and re-apply when the session
    /// is rebuilt, by a model or memory switch).
    disabled: BTreeSet<String>,
}

/// A resource attached to the next message.
#[derive(Debug, Clone)]
struct Attachment {
    /// `server/name`.
    label: String,
    /// The labeled (untrusted-marker) text.
    text: String,
}

/// MCP state of the TUI driver.
///
/// Anything that talks to a server (starting it, `prompts/get`,
/// `resources/read`, disabling or enabling it, which waits for a start in
/// progress) runs in a background task, so the driver keeps handling
/// requests such as Interrupt and Quit meanwhile. A prompt's text comes
/// back on [`take_turns`](Self::take_turns)' channel and is sent with
/// [`Driver::prompt`].
pub(super) struct McpUi {
    servers: Option<Arc<Servers>>,
    labeler: Option<Labeler>,
    shared: Arc<Mutex<Shared>>,
    watcher: Option<JoinHandle<()>>,
    /// Background server work of this session (aborted with it).
    tasks: Vec<JoinHandle<()>>,
    /// Prompt texts ready to be sent as user turns.
    turns_tx: mpsc::UnboundedSender<String>,
    turns_rx: Option<mpsc::UnboundedReceiver<String>>,
}

impl Default for McpUi {
    fn default() -> Self {
        let (turns_tx, turns_rx) = mpsc::unbounded_channel();
        McpUi {
            servers: None,
            labeler: None,
            shared: Arc::default(),
            watcher: None,
            tasks: Vec::new(),
            turns_tx,
            turns_rx: Some(turns_rx),
        }
    }
}

impl McpUi {
    /// Stops following the old session's servers and its background work.
    pub(super) fn stop(&mut self) {
        if let Some(w) = self.watcher.take() {
            w.abort();
        }
        for t in self.tasks.drain(..) {
            t.abort();
        }
    }

    /// The channel of prompt texts that background prompt runs produce;
    /// the driver's loop sends each with [`Driver::prompt`]. Taken once.
    pub(super) fn take_turns(&mut self) -> mpsc::UnboundedReceiver<String> {
        self.turns_rx.take().unwrap_or_else(|| {
            // Already taken: a closed channel, so nothing waits on it.
            mpsc::unbounded_channel().1
        })
    }

    /// The next prompt text of a background prompt run (tests stand in for
    /// the driver's loop with it).
    #[cfg(test)]
    pub(super) async fn next_turn(&mut self) -> Option<String> {
        self.turns_rx.as_mut()?.recv().await
    }

    /// Runs server work in the background.
    fn spawn(&mut self, work: impl std::future::Future<Output = ()> + Send + 'static) {
        self.tasks.retain(|t| !t.is_finished());
        self.tasks.push(tokio::spawn(work));
    }

    /// Prepends the attached resources to `text` (and forgets them).
    pub(super) fn with_attachments(&mut self, text: String) -> String {
        let attached = std::mem::take(&mut lock(&self.shared).attachments);
        if attached.is_empty() {
            return text;
        }
        let mut out = String::new();
        for a in attached {
            out.push_str(&a.text);
            out.push_str("\n\n");
        }
        out.push_str(&text);
        out
    }
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// Server-provided text for a title or description: one line, no control
/// characters, cut.
fn clean(s: &str) -> String {
    let one: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let one = one.split_whitespace().collect::<Vec<_>>().join(" ");
    harness_agent::robustness::truncate_chars(&one, MAX_LABEL_CHARS)
}

/// Cuts model-bound text to [`MAX_CONTENT_CHARS`], saying so.
fn cap(text: &str) -> String {
    if text.chars().count() > MAX_CONTENT_CHARS {
        let cut = harness_agent::robustness::truncate_chars(text, MAX_CONTENT_CHARS);
        format!("{cut}\n[cut at {MAX_CONTENT_CHARS} characters]")
    } else {
        text.to_owned()
    }
}

/// What background server work needs from the driver.
#[derive(Clone)]
struct Ctx {
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::Sender<AppEvent>,
    turns: mpsc::UnboundedSender<String>,
    labeler: Option<Labeler>,
}

impl Ctx {
    fn of(d: &Driver) -> Ctx {
        Ctx {
            shared: d.mcp.shared.clone(),
            tx: d.tx.clone(),
            turns: d.mcp.turns_tx.clone(),
            labeler: d.mcp.labeler.clone(),
        }
    }

    async fn send(&self, c: CoreEvent) {
        let _ = self.tx.send(AppEvent::Core(c)).await;
    }

    async fn notify(&self, level: Level, title: &str, body: Option<String>) {
        self.send(CoreEvent::Notify {
            level,
            title: title.to_owned(),
            body,
        })
        .await;
    }
}

/// Applies the wanted session-disabled state of `h` (from
/// [`Shared::disabled`]) until it holds, and returns it. Re-checking after
/// each change makes quick disable/enable clicks end in the last one's
/// state, whatever order their tasks run in.
async fn sync_disabled(h: &ServerHandle, shared: &Mutex<Shared>) -> bool {
    loop {
        let want = lock(shared).disabled.contains(h.name());
        if want {
            h.disable_for_session().await;
        } else {
            h.enable_for_session().await;
        }
        if lock(shared).disabled.contains(h.name()) == want {
            return want;
        }
    }
}

/// The session started: follow its servers.
pub(super) async fn session_started(
    d: &mut Driver,
    core: &crate::core::Core,
    base_palette: Vec<PaletteEntry>,
    base_slash: Vec<SlashCommand>,
) {
    d.mcp.stop();
    let servers = core.mcp_servers();
    d.mcp.labeler = Some(core.labeler().clone());
    {
        let mut s = lock(&d.mcp.shared);
        s.listings.clear();
        s.base_palette = base_palette;
        s.base_slash = base_slash;
    }
    let disabled = lock(&d.mcp.shared).disabled.clone();
    for name in disabled {
        if let Some(h) = servers.get(&name).cloned() {
            let shared = d.mcp.shared.clone();
            d.mcp.spawn(async move {
                sync_disabled(&h, &shared).await;
            });
        }
    }
    d.mcp.servers = Some(servers.clone());
    if servers.iter().next().is_none() {
        return;
    }
    let (shared, cfg, tx) = (d.mcp.shared.clone(), d.cfg.clone(), d.tx.clone());
    d.mcp.watcher = Some(tokio::spawn(watch(servers, shared, cfg, tx)));
}

/// Follows status changes: refreshes listings, the indicator, palette and
/// completion, and the open panel's text.
async fn watch(
    servers: Arc<Servers>,
    shared: Arc<Mutex<Shared>>,
    cfg: Config,
    tx: mpsc::Sender<AppEvent>,
) {
    let handles: Vec<Arc<ServerHandle>> = servers.iter().cloned().collect();
    let mut rxs: Vec<_> = handles.iter().map(|h| h.watch_status()).collect();
    let mut last_failed: Option<Vec<String>> = None;
    let mut last_entries: Option<(Vec<PaletteEntry>, Vec<SlashCommand>)> = None;
    let mut last_lines: Option<Vec<String>> = None;
    loop {
        for r in &mut rxs {
            r.mark_unchanged();
        }
        refresh(&handles, &shared).await;
        let failed = failed(&handles).await;
        let send = |c: CoreEvent| {
            let tx = tx.clone();
            async move { tx.send(AppEvent::Core(c)).await.is_ok() }
        };
        if last_failed.as_ref() != Some(&failed) {
            if !send(CoreEvent::McpStatus {
                failed: failed.clone(),
            })
            .await
            {
                break;
            }
            last_failed = Some(failed);
        }
        let entries = entries(&shared);
        if last_entries.as_ref() != Some(&entries) {
            let (mut palette, mut slash) = {
                let s = lock(&shared);
                (s.base_palette.clone(), s.base_slash.clone())
            };
            palette.extend(entries.0.iter().cloned());
            slash.extend(entries.1.iter().cloned());
            if !send(CoreEvent::PaletteEntries(palette)).await
                || !send(CoreEvent::SlashCommands(slash)).await
            {
                break;
            }
            last_entries = Some(entries);
        }
        let lines = panel_lines(&handles, &shared, &cfg).await;
        if last_lines.as_ref() != Some(&lines) {
            if !send(CoreEvent::PanelText {
                title: TITLE.into(),
                lines: lines.clone(),
            })
            .await
            {
                break;
            }
            last_lines = Some(lines);
        }
        let changed = {
            let futs = rxs.iter_mut().map(|r| Box::pin(r.changed()));
            futures::future::select_all(futs).await.0
        };
        if changed.is_err() {
            break;
        }
    }
}

/// Re-reads what running servers offer when their status changed since.
async fn refresh(handles: &[Arc<ServerHandle>], shared: &Mutex<Shared>) {
    for h in handles {
        let generation = h.status_generation();
        let running = matches!(h.status().await, Status::Running(_));
        if !running {
            lock(shared).listings.remove(h.name());
            continue;
        }
        let fresh = lock(shared)
            .listings
            .get(h.name())
            .is_some_and(|l| l.generation == generation);
        if fresh {
            continue;
        }
        let tools = h.list_tools().await.ok().map(|t| t.len());
        let prompts = h.list_prompts().await.unwrap_or_else(|e| {
            tracing::warn!(server = h.name(), error = %h.redact(&e.to_string()), "MCP prompts not listed");
            Vec::new()
        });
        let resources = h.list_resources().await.unwrap_or_else(|e| {
            tracing::warn!(server = h.name(), error = %h.redact(&e.to_string()), "MCP resources not listed");
            Vec::new()
        });
        lock(shared).listings.insert(
            h.name().to_owned(),
            Listing {
                generation,
                tools,
                prompts,
                resources,
            },
        );
    }
}

/// Names of the servers that failed.
async fn failed(handles: &[Arc<ServerHandle>]) -> Vec<String> {
    let mut out = Vec::new();
    for h in handles {
        if matches!(
            h.status().await,
            Status::Failing { .. } | Status::GaveUp { .. }
        ) {
            out.push(h.name().to_owned());
        }
    }
    out
}

/// The usage hint of a prompt's arguments, e.g. `<pr> [focus...]`.
fn arg_hint(p: &Prompt) -> Option<String> {
    let spec = prompt_spec("", p);
    spec.arg_hint()
}

/// The palette entries and slash commands of every listed prompt and
/// resource.
fn entries(shared: &Mutex<Shared>) -> (Vec<PaletteEntry>, Vec<SlashCommand>) {
    let s = lock(shared);
    let mut palette = Vec::new();
    let mut slash = Vec::new();
    for (server, l) in &s.listings {
        for p in &l.prompts {
            let name = format!("{server}:{}", p.name);
            let description = clean(
                p.description
                    .as_deref()
                    .or(p.title.as_deref())
                    .unwrap_or("MCP prompt"),
            );
            slash.push(SlashCommand {
                name: name.clone(),
                description: format!("{description} (MCP {server})"),
                args: arg_hint(p),
                scope: Some("mcp".into()),
            });
            palette.push(
                PaletteEntry::new(
                    format!("{PROMPT_PREFIX}{name}"),
                    format!("/{name}"),
                    description,
                    EntryKind::Mcp,
                )
                .with_scope(format!("mcp {server}"))
                .with_args(
                    p.arguments
                        .iter()
                        .map(|a| ArgSpec {
                            name: clean(&a.name),
                            required: a.required,
                            hint: a.description.as_deref().map(clean),
                        })
                        .collect(),
                ),
            );
        }
        for r in &l.resources {
            let mut about = Vec::new();
            if let Some(t) = r.title.as_deref().or(r.description.as_deref()) {
                about.push(clean(t));
            }
            about.push(clean(&r.uri));
            if let Some(m) = &r.mime_type {
                about.push(clean(m));
            }
            palette.push(
                PaletteEntry::new(
                    format!("{RESOURCE_PREFIX}{server}/{}", r.uri),
                    format!("Attach MCP resource: {server}/{}", clean(&r.name)),
                    about.join(" · "),
                    EntryKind::Mcp,
                )
                .with_scope(format!("mcp {server}")),
            );
        }
    }
    (palette, slash)
}

/// `stdio` or `http`.
fn transport(cfg: &McpServer) -> &'static str {
    match cfg.transport {
        McpTransport::Stdio { .. } => "stdio",
        McpTransport::Http { .. } => "http",
    }
}

/// How the server authenticates, without any credential.
fn auth_state(name: &str, cfg: &McpServer, store: &McpTokenStore) -> String {
    match cfg.transport {
        McpTransport::Http { .. } if cfg.uses_oauth() => {
            if store.has_tokens(name) {
                "oauth: authorized".into()
            } else {
                format!("oauth: not authorized (/login mcp {name})")
            }
        }
        McpTransport::Http { .. } if !cfg.headers.is_empty() => "header credentials".into(),
        McpTransport::Stdio { .. } if !cfg.env.is_empty() => "env credentials".into(),
        _ => "no credentials".into(),
    }
}

/// `(status text, failure reason)`.
fn status_text(status: &Status) -> (String, Option<String>) {
    match status {
        Status::Disabled => ("disabled (harness.ini)".into(), None),
        Status::DisabledForSession => ("disabled (this session)".into(), None),
        Status::Idle => ("idle (starts on first use)".into(), None),
        Status::Starting => ("starting".into(), None),
        Status::Running(n) => (format!("connected (MCP {})", n.version), None),
        Status::Failing { failures, last } => (
            format!("failed ({failures}x, retries on use)"),
            Some(last.clone()),
        ),
        Status::GaveUp { last } => ("failed (gave up)".into(), Some(last.clone())),
    }
}

/// The status to show: the wanted session-disabled state wins while a
/// background task is still applying it.
async fn shown_status(h: &ServerHandle, disabled: &BTreeSet<String>) -> Status {
    if !h.config().enabled {
        return Status::Disabled;
    }
    match (disabled.contains(h.name()), h.is_disabled_for_session()) {
        (true, _) => Status::DisabledForSession,
        // Being enabled: it starts on next use.
        (false, true) => Status::Idle,
        (false, false) => h.status().await,
    }
}

/// The `/mcp` panel text.
async fn panel_lines(
    handles: &[Arc<ServerHandle>],
    shared: &Mutex<Shared>,
    cfg: &Config,
) -> Vec<String> {
    let (attached, disabled): (Vec<String>, BTreeSet<String>) = {
        let s = lock(shared);
        (
            s.attachments.iter().map(|a| a.label.clone()).collect(),
            s.disabled.clone(),
        )
    };
    if handles.is_empty() {
        return vec![
            "No MCP servers. Add a [mcp.<name>] section to harness.ini (see docs/mcp.md).".into(),
        ];
    }
    let store = crate::wiring::mcp_token_store(cfg);
    let mut lines = vec![format!(
        "{:<14} {:<30} {:<6} {:>5}  {}",
        "SERVER", "STATUS", "VIA", "TOOLS", "AUTH"
    )];
    for h in handles {
        let status = shown_status(h, &disabled).await;
        let (text, reason) = status_text(&status);
        let listing = lock(shared).listings.get(h.name()).cloned();
        let tools = listing
            .as_ref()
            .and_then(|l| l.tools)
            .map_or_else(|| "-".to_owned(), |n| n.to_string());
        lines.push(format!(
            "{:<14} {:<30} {:<6} {:>5}  {}",
            h.name(),
            text,
            transport(h.config()),
            tools,
            auth_state(h.name(), h.config(), &store)
        ));
        if let Some(r) = reason {
            lines.push(format!("  reason: {}", clean(&h.redact(&r))));
        }
        if let Some(l) = listing.filter(|l| !l.prompts.is_empty() || !l.resources.is_empty()) {
            let prompts: Vec<String> = l
                .prompts
                .iter()
                .map(|p| format!("/{}:{}", h.name(), p.name))
                .collect();
            lines.push(format!(
                "  prompts: {} · resources: {}",
                if prompts.is_empty() {
                    "none".into()
                } else {
                    clean(&prompts.join(", "))
                },
                l.resources.len()
            ));
        }
    }
    lines.push(String::new());
    if attached.is_empty() {
        lines.push(
            "Prompts run as /<server>:<prompt>; attach a resource from the palette (Attach MCP resource)."
                .into(),
        );
    } else {
        lines.push(format!(
            "Attached to your next message: {}",
            attached.join(", ")
        ));
    }
    lines
}

/// The panel buttons for the servers' current (wanted) state.
fn buttons(handles: &[Arc<ServerHandle>], shared: &Mutex<Shared>) -> Vec<PanelButton> {
    let (attached, disabled) = {
        let s = lock(shared);
        (!s.attachments.is_empty(), s.disabled.clone())
    };
    let mut out = Vec::new();
    for h in handles {
        let n = h.name();
        if h.config().enabled {
            out.push(PanelButton::new(
                format!("{RESTART}{n}"),
                format!("restart {n}"),
            ));
            if disabled.contains(n) {
                out.push(PanelButton::new(
                    format!("{ENABLE}{n}"),
                    format!("enable {n}"),
                ));
            } else {
                out.push(PanelButton::new(
                    format!("{DISABLE}{n}"),
                    format!("disable {n}"),
                ));
            }
        }
        out.push(PanelButton::new(format!("{LOGS}{n}"), format!("logs {n}")));
    }
    if attached {
        out.push(PanelButton::new(DETACH, "detach resources"));
    }
    out
}

fn handles(d: &Driver) -> Vec<Arc<ServerHandle>> {
    d.mcp
        .servers
        .as_ref()
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default()
}

/// `/mcp`: the server panel.
pub(super) async fn open(d: &mut Driver) {
    let hs = handles(d);
    let lines = panel_lines(&hs, &d.mcp.shared, &d.cfg).await;
    let buttons = buttons(&hs, &d.mcp.shared);
    d.send(CoreEvent::Panel {
        title: TITLE.into(),
        lines,
        buttons,
    })
    .await;
}

/// True for the `/mcp` panel's buttons.
pub(super) fn is_action(id: &str) -> bool {
    id.starts_with("mcp.") && !id.starts_with(super::mcp_login::BUTTON_PREFIX)
}

/// A `/mcp` panel button.
pub(super) async fn action(d: &mut Driver, id: &str) {
    if id == BACK {
        return open(d).await;
    }
    if id == DETACH {
        lock(&d.mcp.shared).attachments.clear();
        d.notify(Level::Info, "MCP resources detached", None).await;
        return open(d).await;
    }
    let Some((verb, name)) = id.split_once(':') else {
        return;
    };
    let Some(h) = d.mcp.servers.as_ref().and_then(|s| s.get(name)).cloned() else {
        return d
            .notify(
                Level::Error,
                "MCP",
                Some(format!("no MCP server `{name}` in this session")),
            )
            .await;
    };
    let ctx = Ctx::of(d);
    let name = name.to_owned();
    match format!("{verb}:").as_str() {
        RESTART => {
            let was_disabled = lock(&d.mcp.shared).disabled.remove(&name);
            d.notify(Level::Info, &format!("restarting MCP server {name}"), None)
                .await;
            d.mcp.spawn(async move {
                if (was_disabled || h.is_disabled_for_session())
                    && sync_disabled(&h, &ctx.shared).await
                {
                    // Disabled again meanwhile.
                    return;
                }
                let (level, title, body) = match h.restart().await {
                    Ok(n) => (
                        Level::Success,
                        format!("MCP server {name} connected"),
                        Some(format!("MCP {} ({})", n.version, n.era.as_str())),
                    ),
                    Err(e) => (
                        Level::Error,
                        format!("MCP server {name} failed to start"),
                        Some(h.redact(&e.to_string())),
                    ),
                };
                ctx.notify(level, &title, body).await;
            });
            open(d).await;
        }
        DISABLE => {
            lock(&d.mcp.shared).disabled.insert(name.clone());
            d.mcp.spawn(async move {
                // Stopping waits for a start in progress.
                if sync_disabled(&h, &ctx.shared).await {
                    ctx.notify(
                        Level::Info,
                        &format!("MCP server {name} disabled for this session"),
                        Some(format!("its tools fail until you enable {name} in /mcp")),
                    )
                    .await;
                }
            });
            open(d).await;
        }
        ENABLE => {
            lock(&d.mcp.shared).disabled.remove(&name);
            d.mcp.spawn(async move {
                if !sync_disabled(&h, &ctx.shared).await {
                    ctx.notify(
                        Level::Info,
                        &format!("MCP server {name} enabled"),
                        Some("it starts on next use".into()),
                    )
                    .await;
                }
            });
            open(d).await;
        }
        LOGS => logs(d, &h).await,
        _ => {}
    }
}

/// A server's log panel: status, last error and recent stderr (redacted).
async fn logs(d: &mut Driver, h: &Arc<ServerHandle>) {
    let name = h.name();
    let (text, reason) = status_text(&h.status().await);
    let mut lines = vec![format!("status: {text}")];
    if let Some(r) = reason {
        lines.push(format!("last error: {}", h.redact(&r)));
    }
    lines.push(String::new());
    match h.config().transport {
        McpTransport::Stdio { .. } => {
            let tail = h.stderr_tail();
            if tail.is_empty() {
                lines.push("(no stderr output yet)".into());
            } else {
                lines.push("stderr, most recent last (credentials redacted):".into());
                lines.extend(tail.iter().map(|l| h.redact(l)));
            }
        }
        McpTransport::Http { .. } => lines.push(
            "Remote server: no stderr. Requests and errors are in the harness log (target harness_mcp)."
                .into(),
        ),
    }
    let buttons = vec![
        PanelButton::new(BACK, "back"),
        PanelButton::new(format!("{RESTART}{name}"), format!("restart {name}")),
    ];
    d.send(CoreEvent::Panel {
        title: format!("{LOGS_TITLE}{name}"),
        lines,
        buttons,
    })
    .await;
}

/// The command spec a prompt's arguments bind to: positional in the
/// server's order, the last one taking the rest of the line.
fn prompt_spec(name: &str, p: &Prompt) -> CommandSpec {
    let n = p.arguments.len();
    let args = p
        .arguments
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let base = if i + 1 == n {
                CmdArg::rest(a.name.clone())
            } else {
                CmdArg::optional(a.name.clone())
            };
            let base = CmdArg {
                required: a.required,
                ..base
            };
            base.with_help(
                a.description
                    .as_deref()
                    .map_or_else(|| "prompt argument".to_owned(), clean),
            )
        })
        .collect();
    CommandSpec {
        name: name.to_owned(),
        description: p.description.as_deref().map(clean).unwrap_or_default(),
        kind: CommandKind::Skill,
        scope: None,
        args,
        strict: true,
        target: p.name.clone(),
    }
}

/// `(server, prompt, raw arguments)` when `line` is `/<server>:<prompt>`
/// for a configured server and no command of that name exists.
pub(super) fn prompt_target(d: &Driver, line: &str) -> Option<(String, String, String)> {
    let (name, raw) = harness_agent::commands::split_line(line)?;
    let (server, prompt) = name.split_once(':')?;
    if prompt.is_empty() || d.registry.get(name).is_some() {
        return None;
    }
    d.mcp.servers.as_ref()?.get(server)?;
    Some((server.to_owned(), prompt.to_owned(), raw.to_owned()))
}

/// Finds prompt `prompt` of `server`: from the listing, else by asking the
/// server (which starts it).
async fn find_prompt(ctx: &Ctx, h: &ServerHandle, prompt: &str) -> Result<Prompt, String> {
    let listed = lock(&ctx.shared)
        .listings
        .get(h.name())
        .and_then(|l| l.prompts.iter().find(|p| p.name == prompt).cloned());
    if let Some(p) = listed {
        return Ok(p);
    }
    if !matches!(h.status().await, Status::Running(_)) {
        ctx.notify(
            Level::Info,
            &format!("starting MCP server {}", h.name()),
            None,
        )
        .await;
    }
    let all = h
        .list_prompts()
        .await
        .map_err(|e| h.redact(&e.to_string()))?;
    all.iter()
        .find(|p| p.name == prompt)
        .cloned()
        .ok_or_else(|| {
            let names: Vec<String> = all
                .iter()
                .map(|p| format!("/{}:{}", h.name(), p.name))
                .collect();
            format!(
                "MCP server `{}` has no prompt `{prompt}`; its prompts: {}",
                h.name(),
                if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                }
            )
        })
}

/// The palette entry asking for a prompt's arguments.
fn prompt_entry(server: &str, p: &Prompt) -> PaletteEntry {
    let name = format!("{server}:{}", p.name);
    PaletteEntry::new(
        format!("{PROMPT_PREFIX}{name}"),
        format!("/{name}"),
        p.description.as_deref().map(clean).unwrap_or_default(),
        EntryKind::Mcp,
    )
    .with_args(
        p.arguments
            .iter()
            .map(|a| ArgSpec {
                name: clean(&a.name),
                required: a.required,
                hint: a.description.as_deref().map(clean),
            })
            .collect(),
    )
}

/// `/<server>:<prompt> args`: binds the arguments (asking in the palette
/// for missing required ones) and runs the prompt, in the background.
pub(super) async fn run_prompt(d: &mut Driver, server: &str, prompt: &str, raw: &str) {
    let Some(h) = d.mcp.servers.as_ref().and_then(|s| s.get(server)).cloned() else {
        return;
    };
    let ctx = Ctx::of(d);
    let (server, prompt, raw) = (server.to_owned(), prompt.to_owned(), raw.to_owned());
    d.mcp.spawn(async move {
        let p = match find_prompt(&ctx, &h, &prompt).await {
            Ok(p) => p,
            Err(e) => return ctx.notify(Level::Error, "MCP prompt", Some(e)).await,
        };
        let name = format!("{server}:{prompt}");
        let spec = prompt_spec(&name, &p);
        match harness_agent::commands::parse::bind(&name, &spec, &raw) {
            Ok(inv) => {
                let args: BTreeMap<String, String> = inv
                    .args
                    .into_iter()
                    .filter(|(k, _)| p.arguments.iter().any(|a| a.name == *k))
                    .collect();
                execute(&ctx, &h, &p, args).await;
            }
            Err(CommandError::MissingArg { .. }) => {
                ctx.send(CoreEvent::PromptArgs(prompt_entry(&server, &p)))
                    .await
            }
            Err(e) => {
                ctx.notify(Level::Error, "MCP prompt", Some(e.to_string()))
                    .await
            }
        }
    });
}

/// A palette entry of this module (prompt or resource); the server work
/// runs in the background.
pub(super) async fn invocation(d: &mut Driver, id: &str, answers: &[String]) -> bool {
    if let Some(name) = id.strip_prefix(PROMPT_PREFIX) {
        let Some((server, prompt)) = name.split_once(':') else {
            return true;
        };
        let Some(h) = d.mcp.servers.as_ref().and_then(|s| s.get(server)).cloned() else {
            d.notify(
                Level::Error,
                "MCP prompt",
                Some(format!("no MCP server `{server}` in this session")),
            )
            .await;
            return true;
        };
        let ctx = Ctx::of(d);
        let (prompt, answers) = (prompt.to_owned(), answers.to_vec());
        d.mcp.spawn(async move {
            match find_prompt(&ctx, &h, &prompt).await {
                Ok(p) => {
                    let args = p
                        .arguments
                        .iter()
                        .zip(&answers)
                        .filter(|(_, v)| !v.is_empty())
                        .map(|(a, v)| (a.name.clone(), v.clone()))
                        .collect();
                    execute(&ctx, &h, &p, args).await;
                }
                Err(e) => ctx.notify(Level::Error, "MCP prompt", Some(e)).await,
            }
        });
        return true;
    }
    if let Some(rest) = id.strip_prefix(RESOURCE_PREFIX) {
        if let Some((server, uri)) = rest.split_once('/') {
            attach(d, server, uri).await;
        }
        return true;
    }
    false
}

/// Gets the prompt and hands its text to the driver as the next user turn.
async fn execute(ctx: &Ctx, h: &ServerHandle, p: &Prompt, args: BTreeMap<String, String>) {
    let result = match h.get_prompt(&p.name, &args).await {
        Ok(r) => r,
        Err(e) => {
            return ctx
                .notify(
                    Level::Error,
                    &format!("MCP prompt /{}:{} failed", h.name(), p.name),
                    Some(h.redact(&e.to_string())),
                )
                .await;
        }
    };
    let mut blocks = Vec::new();
    for (role, part) in result.parts() {
        let text = match part {
            PromptPart::Text(t) if role == "assistant" => format!("(assistant) {t}"),
            PromptPart::Text(t) => t,
            PromptPart::Resource { uri, text } => {
                // Redacted with the server's credentials before labeling.
                let source = Source::McpResource {
                    server: h.name().to_owned(),
                    uri: h.redact(&uri),
                };
                match &ctx.labeler {
                    Some(l) => l.label(&source, &cap(&h.redact(&text))),
                    None => continue,
                }
            }
            PromptPart::Note(n) => n,
        };
        blocks.push(text);
    }
    if blocks.iter().all(|b| b.trim().is_empty()) {
        return ctx
            .notify(
                Level::Warning,
                &format!("MCP prompt /{}:{} is empty", h.name(), p.name),
                None,
            )
            .await;
    }
    let text = h.redact(&cap(&blocks.join("\n\n")));
    let _ = ctx.turns.send(text);
}

/// Reads resource `uri` of `server` (in the background) and attaches it to
/// the next message.
async fn attach(d: &mut Driver, server: &str, uri: &str) {
    let Some(h) = d.mcp.servers.as_ref().and_then(|s| s.get(server)).cloned() else {
        return d
            .notify(
                Level::Error,
                "MCP resource",
                Some(format!("no MCP server `{server}` in this session")),
            )
            .await;
    };
    let ctx = Ctx::of(d);
    let (server, uri) = (server.to_owned(), uri.to_owned());
    d.mcp
        .spawn(async move { attach_read(&ctx, &h, &server, &uri).await });
}

async fn attach_read(ctx: &Ctx, h: &ServerHandle, server: &str, uri: &str) {
    let Some(labeler) = ctx.labeler.clone() else {
        return;
    };
    let read = match h.read_resource(uri).await {
        Ok(r) => r,
        Err(e) => {
            return ctx
                .notify(
                    Level::Error,
                    &format!("could not read MCP resource {server}/{}", h.redact(uri)),
                    Some(h.redact(&e.to_string())),
                )
                .await;
        }
    };
    let name = lock(&ctx.shared)
        .listings
        .get(server)
        .and_then(|l| l.resources.iter().find(|r| r.uri == uri))
        .map_or_else(|| uri.to_owned(), |r| r.name.clone());
    // The server's own credentials (`env.*`, `header.*`) are redacted
    // before the text reaches the model and the run store, like its tool
    // results.
    let text = labeler.label(
        &Source::McpResource {
            server: server.to_owned(),
            uri: h.redact(uri),
        },
        &cap(&h.redact(&read.text())),
    );
    let label = format!("{server}/{}", clean(&h.redact(&name)));
    lock(&ctx.shared).attachments.push(Attachment {
        label: label.clone(),
        text,
    });
    ctx.notify(
        Level::Success,
        &format!("attached {label}"),
        Some(
            "Sent with your next message, marked as untrusted content. /mcp shows or detaches it."
                .into(),
        ),
    )
    .await;
}
