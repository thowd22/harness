//! `/login mcp [server]` and the palette's "Authorize MCP server" entries:
//! the OAuth browser flow for remote MCP servers, run in the background so
//! the TUI stays responsive while the browser is open.
//!
//! `/login mcp` opens a panel listing the remote servers with their
//! authorization state and an `authorize <name>` button each (clickable,
//! and reachable with Tab and Enter like every panel button).
//! `/login mcp <name>`, a button or a palette entry starts the flow; a
//! toast shows the authorization URL (in case the browser does not open)
//! and then the outcome. Tokens never reach the UI or events.

use std::sync::Arc;

use harness_config::ini::{Config, McpTransport};
use harness_providers::chatgpt::Browser;
use harness_tui::app::{AppEvent, CoreEvent};
use harness_tui::notify::Level;
use harness_tui::palette::{EntryKind, PaletteEntry};
use harness_tui::panel::PanelButton;
use tokio::sync::mpsc;

use super::Driver;

/// Panel button id prefix (`mcp.login:<name>`).
pub const BUTTON_PREFIX: &str = "mcp.login:";
/// Palette entry id prefix (`mcp.login.<name>`).
pub const PALETTE_PREFIX: &str = "mcp.login.";
/// Title of the panel.
pub const TITLE: &str = "MCP authorization";

/// Remote (HTTP) servers of `cfg`, by name.
fn remote_servers(cfg: &Config) -> Vec<(&str, &harness_config::ini::McpServer)> {
    cfg.mcp
        .iter()
        .filter(|(_, s)| matches!(s.transport, McpTransport::Http { .. }))
        .map(|(n, s)| (n.as_str(), s))
        .collect()
}

/// One palette entry per remote MCP server.
pub fn palette_entries(cfg: &Config) -> Vec<PaletteEntry> {
    remote_servers(cfg)
        .into_iter()
        .map(|(name, s)| {
            let McpTransport::Http { url } = &s.transport else {
                unreachable!("remote servers only")
            };
            PaletteEntry::new(
                format!("{PALETTE_PREFIX}{name}"),
                format!("Authorize MCP server: {name}"),
                format!("OAuth sign-in in the browser for {url}"),
                EntryKind::Builtin,
            )
        })
        .collect()
}

/// The config `/login` edits (falls back to the session's).
fn config(d: &Driver) -> Config {
    harness_config::ini::load_file(&d.login_path).unwrap_or_else(|_| d.cfg.clone())
}

/// Panel lines: each remote server with its state.
fn lines(cfg: &Config) -> Vec<String> {
    let store = crate::wiring::mcp_token_store(cfg);
    let servers = remote_servers(cfg);
    if servers.is_empty() {
        return vec!["No remote MCP servers. Add [mcp.<name>] with url = … to harness.ini.".into()];
    }
    servers
        .into_iter()
        .map(|(name, s)| {
            let state = if store.has_tokens(name) {
                "authorized"
            } else if s.uses_oauth() {
                "not authorized"
            } else {
                "auth = none (header.* credentials)"
            };
            format!("{name:<16} {state}")
        })
        .collect()
}

/// `/login mcp [server]`: the panel, and the flow for `server` if given.
pub(super) async fn open(d: &mut Driver, server: Option<&str>) {
    let cfg = config(d);
    let buttons = remote_servers(&cfg)
        .into_iter()
        .map(|(n, _)| PanelButton::new(format!("{BUTTON_PREFIX}{n}"), format!("authorize {n}")))
        .collect();
    d.send(CoreEvent::Panel {
        title: TITLE.into(),
        lines: lines(&cfg),
        buttons,
    })
    .await;
    if let Some(s) = server {
        start(d, s).await;
    }
}

/// Shows the URL in a toast, then opens the real browser.
struct ToastBrowser {
    tx: mpsc::Sender<AppEvent>,
    inner: Arc<dyn Browser>,
    server: String,
}

impl Browser for ToastBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        let _ = self.tx.try_send(AppEvent::Core(CoreEvent::Notify {
            level: Level::Info,
            title: format!("authorize MCP server {}", self.server),
            body: Some(format!(
                "Opening your browser. If it does not open, visit: {url}"
            )),
        }));
        if let Err(e) = self.inner.open(url) {
            tracing::warn!(error = %e, "could not open a browser");
        }
        Ok(())
    }
}

/// Runs the flow for `server` in the background.
pub(super) async fn start(d: &mut Driver, server: &str) {
    let cfg = config(d);
    if !remote_servers(&cfg).iter().any(|(n, _)| *n == server) {
        return d
            .notify(
                Level::Error,
                "MCP authorization",
                Some(format!(
                    "`{server}` is not a remote MCP server in {}; OAuth needs [mcp.{server}] with url = …",
                    d.login_path.display()
                )),
            )
            .await;
    }
    let (tx, path, server) = (d.tx.clone(), d.login_path.clone(), server.to_owned());
    let browser = ToastBrowser {
        tx: tx.clone(),
        inner: d.browser.clone(),
        server: server.clone(),
    };
    d.background.push(tokio::spawn(async move {
        let result = crate::login::mcp::authorize(&path, &server, &browser).await;
        let (level, title, body) = match result {
            Ok(m) => (Level::Success, format!("MCP server {server} authorized"), m),
            Err(e) => (
                Level::Error,
                format!("MCP authorization failed: {server}"),
                e,
            ),
        };
        let _ = tx
            .send(AppEvent::Core(CoreEvent::Notify {
                level,
                title,
                body: Some(body),
            }))
            .await;
        let cfg = harness_config::ini::load_file(&path).ok();
        if let Some(cfg) = cfg {
            let _ = tx
                .send(AppEvent::Core(CoreEvent::PanelText {
                    title: TITLE.into(),
                    lines: lines(&cfg),
                }))
                .await;
        }
    }));
}
