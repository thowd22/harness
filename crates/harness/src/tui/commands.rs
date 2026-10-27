//! Slash commands, palette entries and panel actions of the TUI.
//!
//! Built-ins (`/help`, `/cost`, `/diff`, `/undo`, `/remote`, ...) are
//! carried out here; workflows (`/fix`) run in the background with their
//! gates answered in the chat; skills become a user turn; agent commands
//! start a session with that agent.

use std::sync::Arc;

use async_trait::async_trait;
use harness_agent::commands::{Builtin, CommandKind, CommandRegistry, Invocation as Command};
use harness_agent::cost::{CostGroup, CostReport, UsageLedger};
use harness_agent::session::SessionStore;
use harness_events::schema::NodeId;
use harness_hygiene::ledger::{Ledger, summary_lines};
use harness_hygiene::report::{CheckpointBefore, HygieneReport, ReportInput};
use harness_policy::approvals::{ApprovalAnswer, ApprovalPrompt, ApprovalUi};
use harness_policy::rules::{Action, Mode};
use harness_tui::app::{AppCommand, AppEvent, CoreEvent, DiffSpec};
use harness_tui::diff::CheckpointReverter;
use harness_tui::input::SlashCommand;
use harness_tui::notify::Level;
use harness_tui::palette::{ArgSpec, EntryKind, Invocation, PaletteEntry};
use harness_tui::panel::PanelButton;
use harness_workflow::gates::{ApprovalGate, GateDecision, GateRequest};
use tokio_util::sync::CancellationToken;

use super::{Driver, TuiOptions, approval_request};
use crate::core::Core;

/// Palette id prefix of slash commands.
pub const CMD_PREFIX: &str = "cmd.";
/// Panel action prefix of the remote host test.
pub const REMOTE_TEST: &str = "remote.test:";
/// Title of the remote hosts panel.
pub const REMOTE_TITLE: &str = "Remote hosts";

fn entry_kind(kind: CommandKind) -> EntryKind {
    match kind {
        CommandKind::Builtin(_) => EntryKind::Builtin,
        CommandKind::Workflow => EntryKind::Workflow,
        CommandKind::Skill => EntryKind::Skill,
        CommandKind::Agent => EntryKind::Agent,
    }
}

/// One palette entry per slash command (with its arguments and help),
/// plus recent runs and remote hosts.
pub fn palette_entries(registry: &CommandRegistry, core: &Core) -> Vec<PaletteEntry> {
    let mut v: Vec<PaletteEntry> = registry
        .commands()
        .into_iter()
        .map(|spec| {
            let help = spec.arg_help();
            PaletteEntry::new(
                format!("{CMD_PREFIX}{}", spec.name),
                spec.usage(),
                spec.description.clone(),
                entry_kind(spec.kind),
            )
            .with_scope(spec.scope_label())
            .with_args(
                spec.args
                    .iter()
                    .enumerate()
                    .map(|(i, a)| ArgSpec {
                        name: a.name.clone(),
                        required: a.required,
                        hint: help.get(i).cloned().or_else(|| a.help.clone()),
                    })
                    .collect(),
            )
        })
        .collect();
    let now = time::OffsetDateTime::now_utc();
    let mut runs = harness_tui::history::load_runs(core.data.root(), now);
    runs.truncate(20);
    v.extend(harness_tui::palette::run_entries(
        &runs,
        local_offset(),
        now,
    ));
    v.extend(harness_tui::palette::remote_entries(&core.cfg));
    v.extend(super::mcp_login::palette_entries(&core.cfg));
    if let Ok(list) = SessionStore::new(core.data.sessions()).list() {
        v.extend(crate::sessions::palette_entries(
            &list,
            Some(core.session_id()),
            local_offset(),
            now,
        ));
    }
    v.push(PaletteEntry::new(
        harness_tui::model_picker::PALETTE_ID,
        "Switch model",
        "Pick a model from every provider: context, prices, capabilities and auth",
        EntryKind::Builtin,
    ));
    v
}

/// One `/agents` panel line: the display name and one-line summary first,
/// then the command that switches to it and where it was loaded from.
fn agent_line(agent: &harness_config::agents::Agent, origin: &str) -> String {
    format!(
        "{:<16} {}  (/{} · {origin})",
        agent.display_name(),
        agent.summary(),
        agent.name
    )
}

/// The local UTC offset for run times (UTC when it cannot be detected).
fn local_offset() -> time::UtcOffset {
    crate::sessions::local_offset()
}

/// Slash completion from the registry.
pub fn slash_commands(registry: &CommandRegistry) -> Vec<SlashCommand> {
    registry
        .commands()
        .into_iter()
        .map(|spec| SlashCommand {
            name: spec.name.clone(),
            description: spec.description.clone(),
            args: spec.arg_hint(),
            scope: Some(spec.scope_label().to_owned()),
            choices: spec
                .args
                .first()
                .map(|a| a.choices.clone())
                .unwrap_or_default(),
        })
        .collect()
}

/// Quotes a word for the command tokenizer.
fn quote(word: &str) -> String {
    if !word.is_empty()
        && !word
            .chars()
            .any(|c| c.is_whitespace() || "'\"\\".contains(c))
    {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// A palette entry chosen in the UI.
pub(super) async fn invocation(d: &mut Driver, inv: Invocation) {
    if inv.id == harness_tui::model_picker::PALETTE_ID {
        return model(d, None).await;
    }
    if let Some(name) = inv.id.strip_prefix(CMD_PREFIX) {
        let mut line = format!("/{name}");
        for a in inv.args.iter().filter(|a| !a.is_empty()) {
            line.push(' ');
            line.push_str(&quote(a));
        }
        return slash(d, &line).await;
    }
    if let Some(cmd) = inv
        .id
        .strip_prefix(harness_tui::palette::ids::WORKFLOW_PREFIX)
    {
        return workflow_answers(d, cmd, &inv.args).await;
    }
    if let Some(id) = inv.id.strip_prefix(harness_tui::palette::ids::RUN_PREFIX) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        harness_tui::history::show::run_show(d.data.root(), id, false, &mut out, &mut err);
        let text = String::from_utf8_lossy(if out.is_empty() { &err } else { &out }).into_owned();
        // The TUI names runs, never shows raw ids: drop the `id` line and
        // title the panel with the run's short tag.
        let lines = text
            .lines()
            .filter(|l| !l.starts_with("id "))
            .map(harness_events::names::scrub_ids)
            .collect();
        return d
            .panel(
                &format!("Run {}", harness_events::names::short_tag(id)),
                lines,
            )
            .await;
    }
    if let Some(id) = inv
        .id
        .strip_prefix(harness_tui::palette::ids::SESSION_PREFIX)
    {
        return resume(d, id).await;
    }
    if let Some(server) = inv.id.strip_prefix(super::mcp_login::PALETTE_PREFIX) {
        return super::mcp_login::open(d, Some(server)).await;
    }
    if super::mcp::invocation(d, &inv.id, &inv.args).await {
        return;
    }
    if let Some(host) = inv
        .id
        .strip_prefix(harness_tui::palette::ids::REMOTE_PREFIX)
    {
        return remote(d, Some(host), true).await;
    }
    if let Some(skill) = inv.id.strip_prefix(harness_tui::palette::ids::SKILL_PREFIX) {
        return slash(d, &format!("/{skill} {}", inv.args.join(" "))).await;
    }
    if let Some(agent) = inv.id.strip_prefix(harness_tui::palette::ids::AGENT_PREFIX) {
        return slash(d, &format!("/{agent} {}", inv.args.join(" "))).await;
    }
    d.notify(Level::Warning, "unknown palette entry", Some(inv.id))
        .await;
}

/// Runs a `/command` line.
pub(super) async fn slash(d: &mut Driver, line: &str) {
    if let Some((server, prompt, raw)) = super::mcp::prompt_target(d, line) {
        return super::mcp::run_prompt(d, &server, &prompt, &raw).await;
    }
    let cmd = match d.registry.parse(line) {
        Ok(c) => c,
        Err(harness_agent::commands::CommandError::MissingArg { .. })
            if workflow_spec(d, line).is_some() =>
        {
            // A workflow without its inputs: ask for them in the palette.
            let (target, raw) = workflow_spec(d, line).expect("checked");
            return workflow(d, &target, &raw).await;
        }
        Err(e) => {
            return d.notify(Level::Error, "command", Some(e.to_string())).await;
        }
    };
    match cmd.spec.kind {
        CommandKind::Builtin(b) => builtin(d, b, &cmd).await,
        CommandKind::Workflow => workflow(d, &cmd.spec.target, &cmd.raw).await,
        CommandKind::Skill => skill(d, &cmd).await,
        CommandKind::Agent => {
            let opts = TuiOptions {
                agent: Some(cmd.spec.target.clone()),
                resume: None,
                ..d.opts.clone()
            };
            restart(d, opts).await;
            if !cmd.raw.trim().is_empty() {
                d.prompt(cmd.raw.clone()).await;
            }
        }
    }
}

/// Opens the `/resume` picker over every saved session.
async fn open_session_picker(d: &mut Driver) {
    let store = SessionStore::new(d.data.sessions());
    match store.list() {
        Ok(list) => {
            let current = d.core.as_ref().map(|c| c.session_id().to_owned());
            let rows = crate::sessions::picker_rows(
                &list,
                current.as_deref(),
                local_offset(),
                time::OffsetDateTime::now_utc(),
            );
            d.send(CoreEvent::SessionPicker(rows)).await;
        }
        Err(e) => {
            d.notify(Level::Error, "cannot list sessions", Some(e.to_string()))
                .await
        }
    }
}

/// Resumes the session `query` names (`last`, a name, an id prefix or a
/// tag). An unknown or ambiguous query keeps the current session.
pub(super) async fn resume(d: &mut Driver, query: &str) {
    let store = SessionStore::new(d.data.sessions());
    let meta = match store.find(query) {
        Ok(m) => m,
        Err(e) => {
            return d
                .notify(Level::Error, "cannot resume", Some(e.to_string()))
                .await;
        }
    };
    let opts = TuiOptions {
        resume: Some(meta.id.clone()),
        ..d.opts.clone()
    };
    restart(d, opts).await;
    d.notify(Level::Info, "resumed", Some(meta.display_name()))
        .await;
}

/// `/rename [name]`: renames the session, or shows its name.
async fn rename(d: &mut Driver, name: Option<&str>) {
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    let busy = d.busy();
    let Some(core) = d.core.as_mut() else {
        let why = if busy {
            "the agent is working; rename the session when it finishes"
        } else {
            "no session is open"
        };
        return d
            .notify(Level::Warning, "cannot rename", Some(why.into()))
            .await;
    };
    match name {
        None => {
            let current = core.session().display_name();
            d.notify(
                Level::Info,
                "session name",
                Some(format!("{current} · /rename <name> changes it")),
            )
            .await
        }
        Some(n) => match core.rename(n).await {
            Ok(saved) => {
                let title = format!("Session renamed to {saved}");
                d.notify(Level::Success, &title, None).await
            }
            Err(e) => d.notify(Level::Error, "cannot rename", Some(e)).await,
        },
    }
}

/// Builds the next session and clears the views.
async fn restart(d: &mut Driver, opts: TuiOptions) {
    // End the old session (its final events reach the UI) before the UI
    // resets, so nothing from the old run lands in the new run's state.
    d.finish_core().await;
    d.send(CoreEvent::Reset).await;
    if let Err(e) = d.start(opts).await {
        d.notify(Level::Error, "could not start the session", Some(e))
            .await;
    }
}

async fn builtin(d: &mut Driver, b: Builtin, cmd: &Command) {
    let Some(h) = d.h.clone() else {
        return;
    };
    match b {
        Builtin::Help => {
            let lines = d.registry.help(cmd.get("command"));
            d.panel("Help", lines).await;
        }
        Builtin::Quit => {
            let _ = d.tx.send(AppEvent::Command(AppCommand::Quit)).await;
        }
        Builtin::New => {
            let opts = TuiOptions {
                resume: None,
                ..d.opts.clone()
            };
            restart(d, opts).await;
        }
        Builtin::Resume => match cmd.get("session").map(str::trim).filter(|s| !s.is_empty()) {
            Some(query) => resume(d, query).await,
            None => open_session_picker(d).await,
        },
        Builtin::Rename => rename(d, cmd.get("name")).await,
        Builtin::Mode => match cmd.get("mode") {
            Some(m) => match Mode::parse(m) {
                Some(mode) => {
                    *h.mode.lock().unwrap_or_else(|e| e.into_inner()) = mode;
                    d.opts.mode = Some(mode);
                    d.send(CoreEvent::Mode(mode)).await;
                }
                None => {
                    d.notify(Level::Error, "unknown mode", Some(m.to_owned()))
                        .await
                }
            },
            None => {
                let m = *h.mode.lock().unwrap_or_else(|e| e.into_inner());
                d.notify(Level::Info, "mode", Some(m.as_str().to_owned()))
                    .await
            }
        },
        Builtin::Model => model(d, cmd.get("model")).await,
        Builtin::Effort => super::effort::command(d, cmd.get("effort")).await,
        Builtin::Login if cmd.get("provider") == Some("mcp") => {
            super::mcp_login::open(d, cmd.get("server")).await
        }
        Builtin::Login => {
            d.send(CoreEvent::Login {
                provider: cmd.get("provider").map(str::to_owned),
            })
            .await
        }
        Builtin::Agents => {
            let lines = h
                .catalog
                .agents
                .active()
                .iter()
                .map(|e| agent_line(&e.item, &e.origin.display))
                .collect();
            d.panel("Agents", lines).await;
        }
        Builtin::Workflows => {
            let (lib, _) = crate::workflow::library(&h.catalog, &d.cfg);
            let lines = lib
                .values()
                .map(|w| {
                    let c = harness_workflow::commands::command(w);
                    format!("{:<28} {}", c.usage(), c.description)
                })
                .collect();
            d.panel("Workflows", lines).await;
        }
        Builtin::Skills => {
            let lines = h
                .catalog
                .skills
                .active()
                .iter()
                .map(|e| e.item.listing_line())
                .collect();
            d.panel("Skills", lines).await;
        }
        Builtin::Memory => memory(d, &h, cmd).await,
        Builtin::Runs => {
            let now = time::OffsetDateTime::now_utc();
            let runs = harness_tui::history::load_runs(d.data.root(), now);
            let lines = harness_tui::palette::run_entries(&runs, local_offset(), now)
                .into_iter()
                .map(|e| format!("{}  {}", e.title, e.description))
                .collect();
            d.panel("Runs", lines).await;
        }
        Builtin::Remote => {
            let test = cmd.get("action") == Some("test");
            remote(d, cmd.get("host"), test).await
        }
        Builtin::Cost => {
            let group = cmd.get("group").and_then(CostGroup::parse);
            let lines = match UsageLedger::from_runs(d.data.root(), &h.runs) {
                Ok(ledger) => CostReport::new(&ledger).lines(group),
                Err(e) => vec![format!("cannot read the run log: {e}")],
            };
            d.panel("Cost", lines).await;
        }
        Builtin::Compact => {
            let focus = (!cmd.raw.trim().is_empty()).then(|| cmd.raw.trim().to_owned());
            if d.busy() {
                h.context.request_compaction(focus);
                d.notify(Level::Info, "compaction requested", None).await;
            } else if let Some(core) = d.core.as_mut() {
                match core.compact(focus.as_deref()).await {
                    Ok(r) => {
                        d.notify(Level::Success, "compacted", Some(format!("{r:?}")))
                            .await
                    }
                    Err(e) => d.notify(Level::Warning, "not compacted", Some(e)).await,
                }
            }
        }
        Builtin::Undo => undo(d, &h).await,
        Builtin::Diff => {
            let node = cmd.get("path").map(NodeId::from);
            diff(d, node).await
        }
        Builtin::Context => {
            let mut report =
                harness_tui::inspector::context::ContextReport::new(Some(h.context_window));
            if let Some(system) = &h.system {
                report.add_system_prompt(system);
            }
            report.add_tools(&h.tools);
            let mut lines = vec![format!(
                "{} tokens before history{}",
                report.total(),
                report
                    .percent()
                    .map(|p| format!(" ({p:.1}% of the window)"))
                    .unwrap_or_default()
            )];
            for (kind, tokens) in report.by_kind() {
                lines.push(format!("  {:<14} {tokens}", kind.label()));
            }
            lines.push(String::new());
            for i in &report.items {
                lines.push(format!("{:>7}  {}", i.tokens, i.name));
            }
            d.panel("Context", lines).await;
        }
        Builtin::Config => {
            if cmd.get("action") == Some("check") {
                let (mut out, mut err) = (Vec::new(), Vec::new());
                harness_config::ini::run_check(None, &mut out, &mut err);
                out.extend(err);
                let text = String::from_utf8_lossy(&out).into_owned();
                d.panel("Config check", text.lines().map(str::to_owned).collect())
                    .await;
            } else {
                let text = harness_config::effective::render(&d.cfg, Some(&h.catalog));
                d.panel(
                    "Effective config",
                    text.lines().map(str::to_owned).collect(),
                )
                .await;
            }
        }
        Builtin::Mcp => super::mcp::open(d).await,
        Builtin::Jobs => super::jobs::open(d).await,
        Builtin::Doctor => {
            let input = harness_eval::doctor::Input {
                config: None,
                project: Some(h.cwd.clone()),
                global_dir: None,
            };
            let report = harness_eval::doctor::diagnose(
                &input,
                &harness_eval::doctor::LiveProbes::default(),
            )
            .await;
            d.panel(
                "Doctor",
                report.render().lines().map(str::to_owned).collect(),
            )
            .await;
        }
    }
}

/// A credential submitted in the `/login` modal: verify it, save it, make
/// the provider use it, and answer the modal. The key is added to the
/// redactor whatever the outcome, and never goes into an event. Sign in
/// with ChatGPT runs in the background ([`super::signin`]).
pub(super) async fn login(d: &mut Driver, sub: &harness_tui::login::LoginSubmission) {
    let req = sub.request();
    if matches!(
        req.credential,
        harness_config::ini::edit::Credential::ChatGpt
    ) {
        return super::signin::start_chatgpt(d, sub);
    }
    if let harness_config::ini::edit::Credential::ApiKey(k) = &req.credential {
        remember_secret(d, k.expose());
    }
    let env = |k: &str| std::env::var(k).ok();
    let result = crate::login::apply(&d.login_path, req, d.login_verifier.as_ref(), &env).await;
    after_login(d, req.provider, &result).await;
    d.send(CoreEvent::LoginResult(result)).await;
}

/// After a login of `provider`: pick the new credential up for the next
/// session and the model picker.
pub(super) async fn after_login(d: &mut Driver, provider: &str, result: &Result<String, String>) {
    if result.is_err() {
        return;
    }
    match harness_config::ini::load_file(&d.login_path) {
        Ok(cfg) => {
            if let Some(p) = cfg.providers.get(provider) {
                if let Ok(Some(key)) = p.resolve_api_key() {
                    remember_secret(d, key.expose());
                }
                d.cfg.providers.insert(provider.to_owned(), p.clone());
            }
            d.models = None;
        }
        Err(e) => {
            d.notify(
                Level::Warning,
                "login",
                Some(format!("saved, but harness.ini did not reload: {e}")),
            )
            .await
        }
    }
}

/// Adds `secret` to the redactor used by new sessions.
fn remember_secret(d: &mut Driver, secret: &str) {
    if !d.secrets.iter().any(|s| s == secret) {
        d.secrets.push(secret.to_owned());
        d.redactor = crate::wiring::redactor(&d.secrets);
    }
}

/// The `/model` catalog service, created on first use: it reads the cached
/// live lists only, and forwards every newer catalog to the UI.
fn model_catalog(d: &mut Driver) -> Result<harness_providers::catalog::CatalogService, String> {
    if let Some(s) = &d.models {
        return Ok(s.clone());
    }
    let registry = crate::wiring::registry(&d.cfg)?;
    let service = crate::wiring::model_catalog(&d.cfg, &registry);
    let mut updates = service.subscribe();
    let tx = d.tx.clone();
    d.background.push(tokio::spawn(async move {
        while updates.changed().await.is_ok() {
            let catalog = updates.borrow_and_update().clone();
            if tx
                .send(AppEvent::Core(CoreEvent::ModelCatalog(catalog)))
                .await
                .is_err()
            {
                break;
            }
        }
    }));
    d.models = Some(service.clone());
    Ok(service)
}

/// Opens the `/model` picker from the cached catalog straight away, then
/// refreshes stale live lists in the background.
async fn open_model_picker(d: &mut Driver) {
    let service = match model_catalog(d) {
        Ok(s) => s,
        Err(e) => return d.notify(Level::Error, "model catalog", Some(e)).await,
    };
    let current = d.core.as_ref().map(|c| c.model.reference.clone());
    d.send(CoreEvent::ModelPicker {
        catalog: service.catalog(),
        current,
    })
    .await;
    service.spawn_refresh(false);
}

/// `/model NAME` switches directly; `/model` opens the picker.
pub(super) async fn model(d: &mut Driver, name: Option<&str>) {
    let Some(name) = name else {
        return open_model_picker(d).await;
    };
    if d.busy() {
        d.queue.push_back(format!("/model {name}"));
        return d
            .notify(
                Level::Info,
                "model change queued",
                Some("after this turn".into()),
            )
            .await;
    }
    let Some(core) = d.core.as_mut() else {
        return;
    };
    match core.set_model(name).await {
        Ok(warnings) => {
            let (reference, window) = (core.model.reference.clone(), core.model.context_window);
            // Reasoning support may differ on the new model.
            let effort = super::effort::status_event(core);
            let branch = harness_config::prompts::git_branch(&core.cwd);
            d.opts.model = Some(name.to_owned());
            d.send(CoreEvent::Session {
                model: reference,
                context_window: Some(u64::from(window)),
                branch,
            })
            .await;
            d.send(effort).await;
            for w in warnings {
                d.notify(Level::Warning, "model", Some(w)).await;
            }
        }
        Err(e) => d.notify(Level::Error, "model", Some(e)).await,
    }
}

async fn memory(d: &mut Driver, h: &super::Handles, cmd: &Command) {
    let action = cmd.get("action").unwrap_or("show");
    let query = cmd.get("query").unwrap_or("").to_owned();
    match action {
        "dream" => dream(d, true).await,
        "search" => {
            open_memory(d, h).await;
            memory_request(d, harness_tui::app::MemoryRequest::Search(query)).await;
        }
        _ => open_memory(d, h).await,
    }
}

/// The memory drive (also when memory is off for this session).
fn drive(d: &Driver) -> Result<harness_memory::drive::Drive, String> {
    harness_memory::drive::Drive::from_config(&d.cfg).map_err(|e| e.to_string())
}

/// What the `/memory` browser shows.
fn memory_data(d: &Driver) -> Result<harness_tui::memory::browser::MemoryData, String> {
    use harness_tui::memory::browser::{CommitRow, FileRow, MemoryData};
    let drive = drive(d)?;
    let notes = harness_memory::drive::list_notes(drive.root()).map_err(|e| e.to_string())?;
    let files = notes
        .iter()
        .map(|n| {
            let text = harness_memory::drive::read_note_at(drive.root(), n)
                .ok()
                .flatten()
                .unwrap_or_default();
            let note = harness_memory::drive::note::Note::parse(&text);
            FileRow {
                path: n.as_str().to_owned(),
                title: note.title().unwrap_or(n.as_str()).to_owned(),
                memories: note.bullets().count(),
            }
        })
        .collect();
    let rows = |list: Vec<harness_memory::dream::history::HistoryEntry>| {
        list.into_iter()
            .map(|e| CommitRow {
                commit: e.commit,
                at: e.at,
                subject: e.subject,
            })
            .collect()
    };
    Ok(MemoryData {
        drive: drive.root().display().to_string(),
        enabled: d.cfg.memory.enabled,
        files,
        recent: rows(harness_memory::dream::history::recent_writes(&drive, 30).unwrap_or_default()),
        dreams: rows(harness_memory::dream::history::dreams(&drive, 30).unwrap_or_default()),
    })
}

/// Opens (or refreshes) the `/memory` browser.
async fn open_memory(d: &mut Driver, _h: &super::Handles) {
    match memory_data(d) {
        Ok(data) => d.send(CoreEvent::Memory(data)).await,
        Err(e) => d.notify(Level::Error, "memory", Some(e)).await,
    }
}

/// A checkout to change the drive from: the session's, or one for the
/// browser when memory is off this session.
fn checkout(d: &Driver) -> Result<Arc<harness_memory::sync::SessionCheckout>, String> {
    if let Some(c) = d.h.as_ref().and_then(|h| h.memory.clone()) {
        return Ok(c);
    }
    let drive = drive(d)?;
    harness_memory::sync::SessionCheckout::open(&drive, "browser")
        .map(Arc::new)
        .map_err(|e| e.to_string())
}

/// A `/memory` browser request.
pub(super) async fn memory_request(d: &mut Driver, req: harness_tui::app::MemoryRequest) {
    use harness_tui::app::MemoryRequest as R;
    use harness_tui::memory::browser::SearchRow;
    match req {
        R::Search(q) => {
            let Ok(drive) = drive(d) else { return };
            let path = drive.internal_dir().join(harness_memory::index::INDEX_FILE);
            let mut index = harness_memory::index::SearchIndex::load(&path);
            let _ = index.update(drive.root());
            let rows = index
                .search(&q, harness_memory::tool::MAX_SEARCH_LIMIT)
                .into_iter()
                .map(|hit| SearchRow {
                    path: hit.path.to_string(),
                    title: hit.title,
                    snippets: hit.snippets,
                })
                .collect();
            d.send(CoreEvent::MemorySearch(rows)).await;
        }
        R::Preview(path) => {
            let Ok(drive) = drive(d) else { return };
            let text = harness_memory::drive::NotePath::parse(&path)
                .ok()
                .and_then(|n| {
                    harness_memory::drive::read_note_at(drive.root(), &n)
                        .ok()
                        .flatten()
                })
                .unwrap_or_default();
            d.send(CoreEvent::MemoryDetail { key: path, text }).await;
        }
        R::Diff(sha) => {
            let Ok(drive) = drive(d) else { return };
            let text = harness_memory::dream::history::diff(&drive, &sha)
                .unwrap_or_else(|e| e.to_string());
            d.send(CoreEvent::MemoryDetail { key: sha, text }).await;
        }
        R::Undo(sha) => {
            match checkout(d) {
                Ok(c) => {
                    let commit = sha.clone();
                    let done = tokio::task::spawn_blocking(move || {
                        harness_memory::dream::history::undo(&c, &commit)
                    })
                    .await;
                    match done {
                        Ok(Ok(_)) => {
                            d.send(CoreEvent::MemoryUndone(sha)).await;
                            d.notify(Level::Success, "memory change undone", None).await;
                        }
                        Ok(Err(e)) => {
                            d.notify(Level::Error, "memory undo", Some(e.to_string()))
                                .await
                        }
                        Err(e) => {
                            d.notify(Level::Error, "memory undo", Some(e.to_string()))
                                .await
                        }
                    }
                }
                Err(e) => d.notify(Level::Error, "memory undo", Some(e)).await,
            }
            if let Some(h) = d.h.clone() {
                open_memory(d, &h).await;
            }
        }
        R::SetEnabled(on) => {
            if d.busy() {
                return d
                    .notify(
                        Level::Warning,
                        "the agent is working",
                        Some("change memory after this turn".into()),
                    )
                    .await;
            }
            // The session continues with the same conversation; only the
            // memory tool comes or goes. Notes stay on the drive.
            d.cfg.memory.enabled = on;
            let session = d.core.as_ref().map(|c| c.session_id().to_owned());
            let opts = TuiOptions {
                resume: session,
                ..d.opts.clone()
            };
            // The views are rebuilt from the session's runs.
            d.send(CoreEvent::Reset).await;
            if let Err(e) = d.start(opts).await {
                return d.notify(Level::Error, "memory", Some(e)).await;
            }
            d.notify(
                Level::Info,
                if on {
                    "memory on for this session"
                } else {
                    "memory off for this session"
                },
                Some("notes are kept".into()),
            )
            .await;
            if let Some(h) = d.h.clone() {
                open_memory(d, &h).await;
            }
        }
        R::Dream => dream(d, true).await,
    }
}

async fn undo(d: &mut Driver, h: &super::Handles) {
    let Some(store) = h.checkpoints.clone() else {
        return d
            .notify(
                Level::Warning,
                "nothing to undo",
                Some("checkpoints are off".into()),
            )
            .await;
    };
    if d.busy() {
        return d
            .notify(
                Level::Warning,
                "the agent is working",
                Some("undo after this turn".into()),
            )
            .await;
    }
    match tokio::task::spawn_blocking(move || store.undo()).await {
        Ok(Ok(cp)) => {
            d.notify(Level::Success, "restored", Some(cp.label.clone()))
                .await
        }
        Ok(Err(e)) => d.notify(Level::Warning, "undo", Some(e.to_string())).await,
        Err(e) => d.notify(Level::Error, "undo", Some(e.to_string())).await,
    }
}

/// The artifacts `node` wrote in one of this session's workflow runs.
fn node_artifacts(d: &Driver, node: &NodeId) -> Vec<harness_workflow::artifacts::ArtifactInfo> {
    d.workflow_runs
        .iter()
        .map(|dir| harness_workflow::artifacts::for_node(dir, node))
        .find(|list| !list.is_empty())
        .unwrap_or_default()
}

/// Sends the artifacts of `node` to its detail view.
pub(super) async fn artifacts(d: &mut Driver, node: NodeId) {
    let names: Vec<String> = node_artifacts(d, &node)
        .into_iter()
        .map(|a| a.name)
        .collect();
    if !names.is_empty() {
        d.send(CoreEvent::Artifacts { node, names }).await;
    }
}

/// Shows one artifact of `node` in a panel.
pub(super) async fn open_artifact(d: &mut Driver, node: NodeId, name: &str) {
    let Some(a) = node_artifacts(d, &node)
        .into_iter()
        .find(|a| a.name == name)
    else {
        return d
            .notify(Level::Warning, "artifact not found", Some(name.to_owned()))
            .await;
    };
    let text = match std::fs::read(&a.path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => format!("cannot read {}: {e}", a.path.display()),
    };
    let mut lines = vec![
        format!("{}  {} B  {}", a.name, a.size, a.path.display()),
        String::new(),
    ];
    lines.extend(text.lines().map(str::to_owned));
    d.panel(&format!("Artifact {}", a.name), lines).await;
}

/// Budget badges for the workflow node and every budgeted step, from the
/// run's saved state.
fn budget_badges(
    wf: &harness_workflow::schema::Workflow,
    state: &harness_workflow::rerun::WorkflowState,
) -> Vec<(NodeId, String, harness_tui::theme::Token)> {
    use harness_tui::theme::Token;
    let mut out = Vec::new();
    let b = &state.budget;
    if let (Some(node), Some(badge)) = (&state.node, b.badge()) {
        let token = if b.exceeded {
            Token::Error
        } else if b.warned {
            Token::Warn
        } else {
            Token::Number
        };
        out.push((node.clone(), badge, token));
    }
    for step in &wf.steps {
        let (Some(limit), Some(rec)) = (
            step.budget.as_ref().and_then(|b| b.usd),
            state.steps.get(&step.id),
        ) else {
            continue;
        };
        let Some(node) = &rec.node else { continue };
        let pct = if limit > 0.0 {
            rec.cost_usd / limit * 100.0
        } else {
            100.0
        };
        let warn = step.budget.as_ref().map_or(0.8, |b| b.warn_at) * 100.0;
        let token = if pct >= 100.0 {
            Token::Error
        } else if pct >= warn {
            Token::Warn
        } else {
            Token::Number
        };
        out.push((
            node.clone(),
            format!("${:.2}/${limit:.2} {pct:.0}%", rec.cost_usd),
            token,
        ));
    }
    out
}

/// Consolidates memory in the background: always when `manual`
/// (`/memory dream`), else only when a pass is due (session start).
pub(super) async fn dream(d: &mut Driver, manual: bool) {
    let Some(h) = d.h.clone() else {
        return;
    };
    if !d.cfg.memory.enabled {
        if manual {
            d.notify(Level::Info, "memory is off", None).await;
        }
        return;
    }
    let settings = match crate::dream::settings(&h.catalog) {
        Ok(s) => s,
        Err(e) => return d.notify(Level::Error, "dream settings", Some(e)).await,
    };
    if !manual {
        match crate::dream::due(&d.cfg, &d.data, &settings) {
            Ok(harness_memory::dream::schedule::Due::Yes { .. }) => {}
            Ok(_) => return,
            Err(e) => {
                tracing::info!(target: "harness::dream", error = %e, "no dream check");
                return;
            }
        }
    }
    // Scripted sessions never spend their script on lessons.
    let lessons = if d.opts.fake_script.is_some() {
        None
    } else {
        crate::dream::lesson_model(&d.cfg, None)
    };
    let (cfg, data, redactor, tx) = (
        d.cfg.clone(),
        d.data.clone(),
        d.redactor.clone(),
        d.tx.clone(),
    );
    let project = h.cwd.clone();
    if manual {
        d.notify(Level::Info, "consolidating memory", None).await;
    }
    d.background.push(tokio::spawn(async move {
        let done = crate::dream::run_pass(
            crate::dream::Pass {
                cfg: &cfg,
                data: &data,
                settings,
                redactor,
                lessons,
                project: Some(&project),
                manual,
            },
            &CancellationToken::new(),
        )
        .await;
        let (level, title, body) = match done {
            Ok(r) => (
                Level::Success,
                "memory consolidated".to_owned(),
                Some(r.summary()),
            ),
            Err(e) => (Level::Error, "dream failed".to_owned(), Some(e)),
        };
        let _ = tx
            .send(AppEvent::Core(CoreEvent::Notify { level, title, body }))
            .await;
    }));
}

/// Restores the project to the checkpoint taken before `node` ran (the
/// map's "restore checkpoint" action).
pub(super) async fn restore_checkpoint(d: &mut Driver, node: NodeId) {
    let Some(h) = d.h.clone() else {
        return;
    };
    let Some(store) = h.checkpoints.clone() else {
        return d
            .notify(
                Level::Warning,
                "no checkpoints",
                Some("checkpoints are off".into()),
            )
            .await;
    };
    if d.busy() {
        return d
            .notify(
                Level::Warning,
                "the agent is working",
                Some("restore after this turn".into()),
            )
            .await;
    }
    let id = h
        .turn_checkpoints
        .iter()
        .find(|(n, _)| *n == node)
        .map(|(_, id)| *id);
    let target = node.clone();
    let done = tokio::task::spawn_blocking(move || {
        let id = match id {
            Some(id) => id,
            None => store
                .for_node(&target)?
                .first()
                .map(|c| c.id)
                .ok_or(harness_hygiene::checkpoints::CheckpointError::NothingToUndo)?,
        };
        store.restore(id).map(|_| store.get(id))
    })
    .await;
    match done {
        Ok(Ok(Ok(cp))) => {
            d.notify(Level::Success, "restored", Some(cp.label.clone()))
                .await
        }
        Ok(Ok(Err(e)) | Err(e)) => {
            d.notify(Level::Warning, "restore", Some(e.to_string()))
                .await
        }
        Err(e) => d.notify(Level::Error, "restore", Some(e.to_string())).await,
    }
}

/// Shows the session's or a node's changes, revertable to the checkpoint
/// before the node's (or the session's first) turn.
pub(super) async fn diff(d: &mut Driver, node: Option<NodeId>) {
    let Some(h) = d.h.clone() else {
        return;
    };
    let checkpoint = match &node {
        Some(n) => h
            .turn_checkpoints
            .iter()
            .find(|(t, _)| t == n)
            .map(|(_, id)| *id),
        None => h.turn_checkpoints.first().map(|(_, id)| *id),
    };
    let reverter = match (&h.checkpoints, checkpoint) {
        (Some(store), Some(id)) => Some(Box::new(CheckpointReverter::new(store.clone(), id))
            as Box<dyn harness_tui::diff::FileReverter>),
        _ => None,
    };
    let report = node
        .as_ref()
        .and_then(|n| hygiene_report(d, &h, n).map(|(r, _)| r));
    d.send(CoreEvent::Diff(DiffSpec {
        root: h.cwd.clone(),
        node,
        report,
        reverter,
    }))
    .await;
}

/// The hygiene report of `node` from the run log so far.
fn hygiene_report(
    d: &Driver,
    h: &super::Handles,
    node: &NodeId,
) -> Option<(HygieneReport, Vec<String>)> {
    let events = d.events.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let ledger = Ledger::from_events(&h.cwd, &events);
    let changes = ledger.net_changes(Some(node));
    if changes.is_empty() {
        return None;
    }
    let tracked = harness_hygiene::report::tracked_files(&h.cwd);
    let before = match (
        &h.checkpoints,
        h.turn_checkpoints.iter().find(|(n, _)| n == node),
    ) {
        (Some(store), Some((_, id))) => Some((store.clone(), *id)),
        _ => None,
    };
    let before = before
        .as_ref()
        .map(|(store, id)| CheckpointBefore { store, id: *id });
    let report = HygieneReport::build(ReportInput {
        root: &h.cwd,
        node_id: Some(node),
        changes: &changes,
        scope: None,
        tracked: tracked.as_ref(),
        before: before
            .as_ref()
            .map(|b| b as &dyn harness_hygiene::report::BeforeContent),
    });
    Some((report, summary_lines(&changes)))
}

/// After each turn: the turn's change ledger and hygiene report go to the
/// node detail, the score to the run's metrics, and a summary of any
/// problem to the chat before the turn is reported done.
pub(super) async fn after_turn(d: &mut Driver) {
    let Some(h) = d.h.clone() else {
        return;
    };
    let Some((node, _)) = h.turn_checkpoints.last().cloned().or_else(|| {
        let events = d.events.lock().unwrap_or_else(|e| e.into_inner());
        events
            .iter()
            .rev()
            .find(|e| {
                matches!(
                    e.kind,
                    harness_events::schema::EventKind::NodeStarted {
                        kind: harness_events::schema::NodeKind::Agent,
                        ..
                    }
                ) && e.parent_node_id.is_none()
            })
            .and_then(|e| e.node_id.clone())
            .map(|n| (n, 0))
    }) else {
        return;
    };
    let Some((report, ledger)) = hygiene_report(d, &h, &node) else {
        return;
    };
    let _ = harness_hygiene::report::record_metric(&h.run_dir, &report);
    let lines = report.lines();
    let problems =
        report.score.out_of_scope + report.score.temp_or_debug + report.score.whitespace_only;
    d.send(CoreEvent::Hygiene {
        node,
        ledger,
        report: lines.clone(),
        temp_files: report.temp_files().len(),
    })
    .await;
    if problems > 0 {
        d.notify(
            Level::Warning,
            "hygiene",
            Some(lines.into_iter().take(4).collect::<Vec<_>>().join("\n")),
        )
        .await;
    }
}

/// Deletes the temporary/debug files a node left behind.
pub(super) async fn remove_temp_files(d: &mut Driver, node: NodeId) {
    let Some(h) = d.h.clone() else {
        return;
    };
    let Some((report, _)) = hygiene_report(d, &h, &node) else {
        return d.notify(Level::Info, "no temporary files", None).await;
    };
    match harness_hygiene::report::remove_temp_files(&h.cwd, &report) {
        Ok(removed) if removed.is_empty() => {
            d.notify(Level::Info, "no temporary files", None).await
        }
        Ok(removed) => {
            d.notify(Level::Success, "removed", Some(removed.join(", ")))
                .await
        }
        Err(e) => {
            d.notify(Level::Error, "remove failed", Some(e.to_string()))
                .await
        }
    }
}

/// Reverts a memory change and marks its card.
pub(super) async fn undo_memory(d: &mut Driver, sha: String) {
    let Some(checkout) = d.h.as_ref().and_then(|h| h.memory.clone()) else {
        return d.notify(Level::Warning, "memory is off", None).await;
    };
    let commit = sha.clone();
    match tokio::task::spawn_blocking(move || {
        harness_memory::dream::history::undo(&checkout, &commit)
    })
    .await
    {
        Ok(Ok(_)) => d.send(CoreEvent::MemoryUndone(sha)).await,
        Ok(Err(e)) => {
            d.notify(Level::Error, "memory undo", Some(e.to_string()))
                .await
        }
        Err(e) => {
            d.notify(Level::Error, "memory undo", Some(e.to_string()))
                .await
        }
    }
}

/// `/remote [host]`: the host list with a test button per host; with
/// `test`, tests the host right away.
async fn remote(d: &mut Driver, host: Option<&str>, test: bool) {
    let registry = harness_remote::hosts::HostRegistry::from_config(&d.cfg);
    let cache = &d.probes;
    let buttons = registry
        .names()
        .into_iter()
        .map(|n| PanelButton::new(format!("{REMOTE_TEST}{n}"), format!("test {n}")))
        .collect();
    d.send(CoreEvent::Panel {
        title: REMOTE_TITLE.into(),
        lines: registry.list_lines(cache),
        buttons,
    })
    .await;
    if let (true, Some(h)) = (test, host) {
        panel_action(d, &format!("{REMOTE_TEST}{h}")).await;
    }
}

/// A panel button.
/// Panel button id of an error's primary action (see
/// [`error_action_panel`]).
pub const ERROR_ACTION: &str = "error.action";
/// Title of the panel shown when a turn stops with an actionable error.
pub const ERROR_TITLE: &str = "The agent stopped";

/// Shows a turn-stopping error whose way out is `action` (for example
/// *Manage usage* for a ChatGPT plan usage limit) as a panel whose first,
/// focused button runs the action. Nothing falls back to another credential.
pub(super) async fn error_action_panel(
    d: &mut Driver,
    message: &str,
    action: harness_events::schema::ErrorAction,
) {
    let mut lines: Vec<String> = message.lines().map(str::to_owned).collect();
    lines.push(String::new());
    lines.push(format!("{}: {}", action.label, action.url));
    let buttons = vec![PanelButton::new(ERROR_ACTION, action.label.clone())];
    d.error_action = Some(action);
    d.send(CoreEvent::Panel {
        title: ERROR_TITLE.into(),
        lines,
        buttons,
    })
    .await;
}

pub(super) async fn panel_action(d: &mut Driver, id: &str) {
    if id == ERROR_ACTION {
        let Some(action) = d.error_action.clone() else {
            return;
        };
        return match d.browser.open(&action.url) {
            Ok(()) => {
                d.notify(Level::Info, "opened in the browser", Some(action.url))
                    .await
            }
            Err(e) => {
                d.notify(
                    Level::Error,
                    "could not open the browser",
                    Some(format!("{e}; open {} yourself", action.url)),
                )
                .await
            }
        };
    }
    if let Some(server) = id.strip_prefix(super::mcp_login::BUTTON_PREFIX) {
        return super::mcp_login::start(d, server).await;
    }
    if super::mcp::is_action(id) {
        return super::mcp::action(d, id).await;
    }
    if super::jobs::is_action(id) {
        return super::jobs::action(d, id).await;
    }
    if super::effort::is_action(id) {
        return super::effort::action(d, id).await;
    }
    let Some(name) = id.strip_prefix(REMOTE_TEST) else {
        return;
    };
    let registry = harness_remote::hosts::HostRegistry::from_config(&d.cfg);
    let host = match registry.get(name) {
        Ok(h) => h.clone(),
        Err(e) => return d.notify(Level::Error, "remote", Some(e.to_string())).await,
    };
    d.send(CoreEvent::PanelText {
        title: REMOTE_TITLE.into(),
        lines: vec![format!("testing {name} ...")],
    })
    .await;
    let report = crate::remote::test(&host, d.data.root(), &d.secrets, &d.probes).await;
    let health = &report.health;
    if let Some(token) = harness_tui::theme::Token::from_name(health.token()) {
        d.send(CoreEvent::HostBadge {
            host: name.to_owned(),
            badge: health.badge().to_owned(),
            token,
        })
        .await;
    }
    let mut lines = registry.list_lines(&d.probes);
    lines.push(String::new());
    lines.extend(report.lines());
    d.send(CoreEvent::PanelText {
        title: REMOTE_TITLE.into(),
        lines,
    })
    .await;
}

/// Answers workflow approval steps in the chat.
struct ChatGates(harness_policy::approvals::ChannelUi);

#[async_trait]
impl ApprovalGate for ChatGates {
    async fn decide(&self, req: GateRequest, cancel: &CancellationToken) -> GateDecision {
        let mut message = req.message.clone();
        for a in &req.artifacts {
            message.push_str(&format!("\nartifact: {}", a.name));
        }
        let request = approval_request(
            &req.approval_id,
            &format!("gate.{}", req.step),
            Some(req.node.clone()),
            Action::Tool {
                name: format!("approve step {}", req.step),
            },
            &message,
        );
        let ask = self.0.ask(ApprovalPrompt {
            batch: 0,
            requests: vec![request],
        });
        tokio::select! {
            answers = ask => match answers.into_iter().next() {
                Some((_, ApprovalAnswer::Approve(_))) => GateDecision::approve("user"),
                Some((_, ApprovalAnswer::Deny { reason })) => {
                    GateDecision::deny("user", reason.unwrap_or_else(|| "denied".into()))
                }
                None => GateDecision::deny("user", "no answer"),
            },
            _ = cancel.cancelled() => GateDecision::deny("user", "cancelled"),
        }
    }
}

/// The workflow named by a `/command args` line: `(workflow, args)`.
fn workflow_spec(d: &Driver, line: &str) -> Option<(String, String)> {
    let (name, raw) = harness_agent::commands::split_line(line)?;
    let spec = d.registry.get(name)?;
    (spec.kind == CommandKind::Workflow).then(|| (spec.target.clone(), raw.to_owned()))
}

/// `/<workflow> args`: runs it, or asks for missing inputs in the palette.
async fn workflow(d: &mut Driver, target: &str, raw: &str) {
    let Some(h) = d.h.clone() else {
        return;
    };
    let (lib, _) = crate::workflow::library(&h.catalog, &d.cfg);
    let wf = match crate::workflow::find(&lib, target) {
        Ok(w) => w.clone(),
        Err(e) => return d.notify(Level::Error, "workflow", Some(e)).await,
    };
    let parsed = match harness_workflow::commands::parse_args(&wf, raw) {
        Ok(p) => p,
        Err(e) => return d.notify(Level::Error, "workflow", Some(e)).await,
    };
    if !parsed.missing.is_empty() {
        let args = parsed
            .missing
            .iter()
            .map(|m| ArgSpec {
                name: m.prompt(),
                required: true,
                hint: Some(m.hint()),
            })
            .collect();
        let entry = PaletteEntry::new(
            format!(
                "{}{}",
                harness_tui::palette::ids::WORKFLOW_PREFIX,
                wf.command
            ),
            format!("/{}", wf.command),
            wf.description.clone().unwrap_or_default(),
            EntryKind::Workflow,
        )
        .with_args(args);
        d.pending_workflow = Some((wf.command.clone(), parsed));
        return d.send(CoreEvent::PromptArgs(entry)).await;
    }
    match harness_workflow::commands::complete(&wf, &parsed, &[]) {
        Ok(inputs) => run_workflow(d, wf, inputs).await,
        Err(e) => d.notify(Level::Error, "workflow", Some(e)).await,
    }
}

/// The palette's answers for the inputs a workflow command was missing.
async fn workflow_answers(d: &mut Driver, command: &str, answers: &[String]) {
    let Some(h) = d.h.clone() else {
        return;
    };
    let (lib, _) = crate::workflow::library(&h.catalog, &d.cfg);
    let wf = match crate::workflow::find(&lib, command) {
        Ok(w) => w.clone(),
        Err(e) => return d.notify(Level::Error, "workflow", Some(e)).await,
    };
    let parsed = match d.pending_workflow.take() {
        Some((c, p)) if c == wf.command => p,
        _ => match harness_workflow::commands::parse_args(&wf, "") {
            Ok(p) => p,
            Err(e) => return d.notify(Level::Error, "workflow", Some(e)).await,
        },
    };
    let named: Vec<(String, String)> = parsed
        .missing
        .iter()
        .zip(answers)
        .map(|(m, a)| (m.name.clone(), a.clone()))
        .collect();
    match harness_workflow::commands::complete(&wf, &parsed, &named) {
        Ok(inputs) => run_workflow(d, wf, inputs).await,
        Err(e) => d.notify(Level::Error, "workflow", Some(e)).await,
    }
}

async fn run_workflow(
    d: &mut Driver,
    wf: harness_workflow::schema::Workflow,
    inputs: serde_json::Map<String, serde_json::Value>,
) {
    let Some(h) = d.h.clone() else {
        return;
    };
    let (lib, _) = crate::workflow::library(&h.catalog, &d.cfg);
    let fake = match &d.opts.fake_script {
        Some(s) => crate::wiring::fake_provider(s).ok(),
        None => None,
    };
    let mode = *h.mode.lock().unwrap_or_else(|e| e.into_inner());
    let built = crate::workflow::build(crate::workflow::Setup {
        cfg: &d.cfg,
        data: &d.data,
        secrets: d.secrets.clone(),
        redactor: d.redactor.clone(),
        cwd: &h.cwd,
        catalog: h.catalog.clone(),
        library: lib,
        gates: Arc::new(ChatGates(d.approvals.clone())),
        approvals: Arc::new(d.approvals.clone()),
        allow: Vec::new(),
        mode,
        model: d.opts.model.clone(),
        max_turns: None,
        fake,
        headless: false,
        memory: h.memory.clone(),
        memory_cap: wf.memory,
        jobs: h.jobs.clone(),
    })
    .await;
    let built = match built {
        Ok(b) => b,
        Err(e) => return d.notify(Level::Error, "workflow", Some(e)).await,
    };
    let tx = d.tx.clone();
    let run_dir = built.recorder.dir().to_path_buf();
    d.workflow_runs.push(run_dir.clone());
    // Events to the UI; budget badges after every node that finishes.
    let mut rx = built.recorder.subscribe();
    let forward = {
        let (tx, wf) = (tx.clone(), wf.clone());
        tokio::spawn(async move {
            loop {
                let ev = match rx.recv().await {
                    Ok(ev) => ev,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                let finished = matches!(
                    ev.kind,
                    harness_events::schema::EventKind::NodeFinished { .. }
                );
                if tx.send(AppEvent::Run(Box::new(ev))).await.is_err() {
                    break;
                }
                if finished
                    && let Ok(state) = harness_workflow::rerun::WorkflowState::load(&run_dir)
                {
                    for (node, badge, token) in budget_badges(&wf, &state) {
                        let _ = tx
                            .send(AppEvent::Core(CoreEvent::NodeBadge { node, badge, token }))
                            .await;
                    }
                }
            }
        })
    };
    d.notify(Level::Info, &format!("workflow {} started", wf.name), None)
        .await;
    let task = tokio::spawn(async move {
        let crate::workflow::Built {
            executor,
            recorder,
            jobs,
            ..
        } = built;
        let outcome = executor
            .run(&wf, &inputs, None, &CancellationToken::new())
            .await;
        // The run's background jobs end with it (recorded before the log
        // closes, so the status line sees them end).
        jobs.shutdown().await;
        let _ = recorder.finish().await;
        drop((executor, recorder));
        // Let the forwarder deliver the last events before it stops.
        let mut forward = forward;
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut forward)
            .await
            .is_err()
        {
            forward.abort();
        }
        let (level, what) = match outcome.status {
            harness_workflow::rerun::WorkflowStatus::Succeeded => (Level::Success, "succeeded"),
            harness_workflow::rerun::WorkflowStatus::Cancelled => (Level::Warning, "cancelled"),
            _ => (Level::Error, "failed"),
        };
        let _ = tx
            .send(AppEvent::Core(CoreEvent::Notify {
                level,
                title: format!("workflow {} {what}", wf.name),
                body: outcome.error,
            }))
            .await;
    });
    d.background.push(task);
}

/// `/<skill> args`: the expanded skill becomes the next user turn.
async fn skill(d: &mut Driver, cmd: &Command) {
    let Some(h) = d.h.clone() else {
        return;
    };
    let text = match h.catalog.skills.get(&cmd.spec.target) {
        Some(s) => s.expand(&cmd.raw),
        None => {
            return d
                .notify(Level::Error, "unknown skill", Some(cmd.spec.target.clone()))
                .await;
        }
    };
    d.prompt(text).await;
}
