//! MCP tools for agents: selection, context cost, results and refresh.
//!
//! An agent gets an MCP tool only when its YAML selects it
//! (`tools: [mcp:github/create_issue, mcp:db/*]`, see
//! [`harness_config::mcp_tools::McpSelector`]); servers come only from
//! harness.ini. [`McpToolbox`] turns the selection into agent tools:
//!
//! - **Direct tools.** While the selected tools' schemas fit
//!   `[context] mcp_schema_budget` (estimated like every tool definition),
//!   each is offered under its own name, `mcp__<server>__<tool>`, with the
//!   server's schema.
//! - **Search-and-call.** Over the budget, a single [`GATEWAY_TOOL`]
//!   (`mcp_tools`) stands in for all of them: `search` returns matching
//!   tools with their schemas (loading them into the conversation), and
//!   `call` runs one with `arguments`. Its own definition is small and
//!   fixed, so the initial context stays bounded however large the server.
//!
//! Each call goes through the policy engine as `mcp:<server>/<tool>`
//! ([`AgentTool::mcp_target`]); the agent loop records an `mcp_call` event
//! naming the server and tool. Results are redacted with the server's
//! credentials, size-capped with spill files, and labeled untrusted by the
//! loop (`trust="untrusted" origin="mcp server"`).
//!
//! **Stable prefix.** What an agent is offered is fixed the first time the
//! toolbox builds it ([`McpToolbox::offer`]) and kept for the session, so
//! the request's tool definitions never change mid-session. When a server
//! announces a tool-list change the toolbox re-reads the list at once
//! (calls and searches use the new list, removed tools return a clear
//! error) and records an `mcp_tools_changed` event; new direct tools appear
//! in the next session.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use harness_agent::context::estimate_tools;
use harness_agent::dispatch::{AgentTool, McpTarget};
use harness_agent::robustness::{self, ToolErrorMessage};
use harness_agent::subagents::ExtraTools;
use harness_config::agents::Agent;
use harness_config::mcp_tools::McpSelector;
use harness_events::schema::EventKind;
use harness_events::store::{EventDraft, RunRecorder};
use harness_policy::mcp::{GATEWAY_TOOL, MODEL_TOOL_PREFIX, ReadOnlyTools};
use harness_providers::model::ToolSpec;
use harness_tools::spill::Spiller;
use harness_tools::tool::{ToolContext, ToolError, ToolErrorKind, ToolOutput};
use serde_json::{Value, json};

use crate::protocol::{CallToolResult, Tool};
use crate::server::{ServerHandle, Servers};

/// Longest tool description passed on from a server (characters).
pub const MAX_DESCRIPTION_CHARS: usize = 1024;
/// Most tools a `search` returns with their schemas.
pub const SEARCH_RESULTS: usize = 8;
/// Most tools an empty `search` lists by name.
pub const LIST_RESULTS: usize = 100;
/// Longest tool name offered to a model (the strictest provider limit).
pub const MAX_MODEL_NAME: usize = 64;
/// Result size limit when no spiller is configured (bytes).
pub const DEFAULT_MAX_RESULT_BYTES: usize = 16_000;

/// Toolbox settings.
#[derive(Clone)]
pub struct ToolboxOptions {
    /// `[context] mcp_schema_budget`: token budget for the selected tools'
    /// schemas before they are replaced by the search-and-call tool.
    pub schema_budget: u64,
    /// Servers whose `readOnlyHint` annotations the user trusts
    /// (`[policy] mcp.<server>.trust_annotations`); their read-only tools
    /// may run in parallel and are fed to the policy engine.
    pub trusted_annotations: BTreeSet<String>,
    /// Where large results go (`spill:<id>` pointers); `None` cuts them at
    /// [`DEFAULT_MAX_RESULT_BYTES`].
    pub spill: Option<Spiller>,
}

impl Default for ToolboxOptions {
    fn default() -> Self {
        Self {
            schema_budget: harness_config::ini::config::DEFAULT_MCP_SCHEMA_BUDGET,
            trusted_annotations: BTreeSet::new(),
            spill: None,
        }
    }
}

impl ToolboxOptions {
    /// Settings from harness.ini (`[context] mcp_schema_budget`,
    /// `[policy] mcp.<server>.trust_annotations`), spilling with `spill`.
    pub fn from_config(cfg: &harness_config::ini::Config, spill: Option<Spiller>) -> Self {
        Self {
            schema_budget: cfg.context.mcp_schema_budget,
            trusted_annotations: cfg
                .policy
                .mcp
                .iter()
                .filter(|(_, r)| r.trust_annotations)
                .map(|(s, _)| s.clone())
                .collect(),
            spill,
        }
    }
}

/// One server's tool list as last read.
#[derive(Debug, Clone, Default)]
struct Listing {
    generation: u64,
    tools: Vec<Tool>,
    error: Option<String>,
}

struct Inner {
    servers: Arc<Servers>,
    opts: ToolboxOptions,
    read_only: Arc<ReadOnlyTools>,
    listings: tokio::sync::Mutex<BTreeMap<String, Listing>>,
    offers: Mutex<HashMap<String, Vec<Arc<dyn AgentTool>>>>,
    /// Every selector offered so far (for change events).
    selected: Mutex<BTreeSet<McpSelector>>,
    recorder: Mutex<Option<RunRecorder>>,
    warnings: Mutex<Vec<String>>,
    watchers: Mutex<BTreeMap<String, tokio::task::JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(w) = self.watchers.get_mut() {
            for t in w.values() {
                t.abort();
            }
        }
    }
}

/// The MCP tools of one session (see the module docs). Cheap to clone.
#[derive(Clone)]
pub struct McpToolbox {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for McpToolbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolbox")
            .field("schema_budget", &self.inner.opts.schema_budget)
            .finish_non_exhaustive()
    }
}

/// Splits `server/tool`.
fn split_ref(s: &str) -> Option<(&str, &str)> {
    let (a, b) = s.trim().split_once('/')?;
    (!a.is_empty() && !b.is_empty()).then_some((a, b))
}

/// `name` restricted to the characters every provider accepts.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A short stable hash for name disambiguation.
fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:08x}", h & 0xffff_ffff)
}

/// The name a tool is offered under: `mcp__<server>__<tool>`, cut to
/// [`MAX_MODEL_NAME`] characters with a hash suffix when longer.
pub fn model_tool_name(server: &str, tool: &str) -> String {
    let full = format!(
        "{MODEL_TOOL_PREFIX}{}__{}",
        sanitize(server),
        sanitize(tool)
    );
    if full.len() <= MAX_MODEL_NAME {
        return full;
    }
    let hash = short_hash(&format!("{server}/{tool}"));
    format!("{}_{hash}", &full[..MAX_MODEL_NAME - hash.len() - 1])
}

/// True when the server marks the tool `readOnlyHint: true`.
fn read_only_hint(tool: &Tool) -> bool {
    tool.raw
        .get("annotations")
        .and_then(|a| a.get("readOnlyHint"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The tool's argument schema, always an object schema.
fn input_schema(tool: &Tool) -> Value {
    let mut s = match &tool.input_schema {
        Value::Object(m) => Value::Object(m.clone()),
        _ => json!({}),
    };
    if s.get("type").is_none() {
        s["type"] = json!("object");
    }
    s
}

/// The description offered to the model (untrusted server text, cut).
fn description(server: &str, tool: &Tool) -> String {
    let d = tool.description.as_deref().unwrap_or("").trim();
    let d = robustness::truncate_chars(d, MAX_DESCRIPTION_CHARS);
    if d.is_empty() {
        format!("Tool `{}` of MCP server `{server}`.", tool.name)
    } else {
        format!("{d} (MCP server `{server}`)")
    }
}

impl McpToolbox {
    /// A toolbox over `servers`. `read_only` is shared with the policy
    /// engine (`harness_policy::mcp::McpRules`).
    pub fn new(servers: Arc<Servers>, opts: ToolboxOptions, read_only: Arc<ReadOnlyTools>) -> Self {
        Self {
            inner: Arc::new(Inner {
                servers,
                opts,
                read_only,
                listings: tokio::sync::Mutex::default(),
                offers: Mutex::default(),
                selected: Mutex::default(),
                recorder: Mutex::default(),
                warnings: Mutex::default(),
                watchers: Mutex::default(),
            }),
        }
    }

    /// Records `mcp_tools_changed` events to `recorder` from now on.
    pub fn set_recorder(&self, recorder: RunRecorder) {
        if let Ok(mut r) = self.inner.recorder.lock() {
            *r = Some(recorder);
        }
    }

    /// The servers.
    pub fn servers(&self) -> &Arc<Servers> {
        &self.inner.servers
    }

    /// Problems found while loading tool lists (unknown or failing
    /// servers, selectors that match nothing), each reported once.
    pub fn take_warnings(&self) -> Vec<String> {
        self.inner
            .warnings
            .lock()
            .map(|mut w| std::mem::take(&mut *w))
            .unwrap_or_default()
    }

    fn warn(&self, w: String) {
        tracing::warn!(target: "harness_mcp::tools", "{w}");
        if let Ok(mut v) = self.inner.warnings.lock()
            && !v.contains(&w)
        {
            v.push(w);
        }
    }

    /// Reads the tool lists of every server `selectors` name that has not
    /// been read yet (or changed since), concurrently.
    pub async fn prepare(&self, selectors: &[McpSelector]) {
        let servers: BTreeSet<&str> = selectors.iter().map(|s| s.server.as_str()).collect();
        futures::future::join_all(servers.into_iter().map(|s| self.ensure(s))).await;
    }

    /// The listing of `server`, read now if missing or out of date.
    async fn ensure(&self, server: &str) -> Option<Listing> {
        let Some(handle) = self.inner.servers.get(server).cloned() else {
            self.warn(format!(
                "MCP server `{server}` is not defined in harness.ini; add a [mcp.{server}] section"
            ));
            return None;
        };
        if !handle.config().enabled {
            self.warn(format!(
                "MCP server `{server}` is disabled in harness.ini (enabled = false); its tools are not offered"
            ));
            return None;
        }
        let current = handle.tools_generation();
        {
            let l = self.inner.listings.lock().await;
            // A failed read is retried on the next use.
            if let Some(x) = l
                .get(server)
                .filter(|x| x.generation == current && x.error.is_none())
            {
                return Some(x.clone());
            }
        }
        Some(self.reload(&handle).await)
    }

    /// Re-reads `handle`'s tools, stores them, updates the read-only set and
    /// records a change event when a selected tool appeared or vanished.
    async fn reload(&self, handle: &Arc<ServerHandle>) -> Listing {
        let server = handle.name().to_owned();
        let generation = handle.tools_generation();
        let listing = match handle.list_tools().await {
            Ok(tools) => Listing {
                generation,
                tools,
                error: None,
            },
            Err(e) => {
                let msg = handle.redact(&e.to_string());
                self.warn(format!(
                    "MCP server `{server}` did not list its tools: {msg}"
                ));
                Listing {
                    generation,
                    tools: Vec::new(),
                    error: Some(msg),
                }
            }
        };
        if self.inner.opts.trusted_annotations.contains(&server) {
            self.inner.read_only.set(
                &server,
                listing
                    .tools
                    .iter()
                    .filter(|t| read_only_hint(t))
                    .map(|t| t.name.clone()),
            );
        }
        let previous = {
            let mut l = self.inner.listings.lock().await;
            let prev = l.get(&server).cloned();
            // A failed re-read keeps the last good list.
            if listing.error.is_none() || prev.is_none() {
                l.insert(server.clone(), listing.clone());
            } else if let Some(p) = l.get_mut(&server) {
                p.generation = generation;
            }
            prev
        };
        if let Some(prev) = previous.filter(|_| listing.error.is_none()) {
            self.record_change(&server, &prev.tools, &listing.tools);
        }
        self.watch(handle);
        listing
    }

    fn record_change(&self, server: &str, before: &[Tool], after: &[Tool]) {
        let selected = self
            .inner
            .selected
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default();
        let picked = |tools: &[Tool]| -> BTreeSet<String> {
            tools
                .iter()
                .filter(|t| selected.iter().any(|s| s.matches(server, &t.name)))
                .map(|t| t.name.clone())
                .collect()
        };
        let (b, a) = (picked(before), picked(after));
        let schemas = |tools: &[Tool]| -> BTreeMap<String, Value> {
            tools
                .iter()
                .map(|t| (t.name.clone(), t.input_schema.clone()))
                .collect()
        };
        if a == b && schemas(before) == schemas(after) {
            return;
        }
        let added: Vec<String> = a.difference(&b).cloned().collect();
        let removed: Vec<String> = b.difference(&a).cloned().collect();
        tracing::info!(target: "harness_mcp::tools", server, ?added, ?removed, "MCP tools changed");
        let rec = self.inner.recorder.lock().ok().and_then(|r| r.clone());
        if let Some(rec) = rec {
            let kind = EventKind::McpToolsChanged {
                server: server.to_owned(),
                added,
                removed,
                tools: u32::try_from(a.len()).unwrap_or(u32::MAX),
            };
            if let Err(e) = rec.emit(EventDraft::run(kind)) {
                tracing::warn!(target: "harness_mcp::tools", error = %e, "could not record an MCP tool change");
            }
        }
    }

    /// Re-reads a server's tools whenever it announces a change.
    fn watch(&self, handle: &Arc<ServerHandle>) {
        let Ok(mut w) = self.inner.watchers.lock() else {
            return;
        };
        if w.contains_key(handle.name()) || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let handle = handle.clone();
        let mut rx = handle.watch_tools();
        rx.mark_unchanged();
        w.insert(
            handle.name().to_owned(),
            tokio::spawn(async move {
                while rx.changed().await.is_ok() {
                    let Some(inner) = weak.upgrade() else { break };
                    let tb = McpToolbox { inner };
                    tb.reload(&handle).await;
                }
            }),
        );
    }

    /// Re-reads every listed server whose tools changed since. The
    /// background watchers normally do this already; call it at turn
    /// boundaries to be sure.
    pub async fn refresh(&self) {
        let names: Vec<String> = self.inner.listings.lock().await.keys().cloned().collect();
        for name in names {
            if let Some(h) = self.inner.servers.get(&name).cloned() {
                let current = h.tools_generation();
                let stale = self
                    .inner
                    .listings
                    .lock()
                    .await
                    .get(&name)
                    .is_none_or(|l| l.generation != current);
                if stale {
                    self.reload(&h).await;
                }
            }
        }
    }

    /// The selected tools available now, in selector order, without
    /// duplicates. Only servers already read are consulted.
    async fn resolve(&self, selectors: &[McpSelector]) -> Vec<(String, Tool)> {
        let l = self.inner.listings.lock().await;
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for sel in selectors {
            let Some(listing) = l.get(&sel.server) else {
                continue;
            };
            for t in &listing.tools {
                if sel.matches(&sel.server, &t.name)
                    && seen.insert((sel.server.clone(), t.name.clone()))
                {
                    out.push((sel.server.clone(), t.clone()));
                }
            }
        }
        out
    }

    /// The tools offered for `key` (an agent and its selectors): built on
    /// first use from the listings read so far, then the same for the rest
    /// of the session. Direct tools when their schemas fit the budget,
    /// else the search-and-call tool.
    pub async fn offer(&self, key: &str, selectors: &[McpSelector]) -> Vec<Arc<dyn AgentTool>> {
        if selectors.is_empty() {
            return Vec::new();
        }
        if let Some(t) = self
            .inner
            .offers
            .lock()
            .ok()
            .and_then(|o| o.get(key).cloned())
        {
            return t;
        }
        if let Ok(mut s) = self.inner.selected.lock() {
            s.extend(selectors.iter().cloned());
        }
        let resolved = self.resolve(selectors).await;
        for sel in selectors {
            if !sel.is_pattern() && !resolved.iter().any(|(s, t)| sel.matches(s, &t.name)) {
                let listed = self.inner.listings.lock().await.contains_key(&sel.server);
                if listed {
                    self.warn(format!(
                        "`{sel}` matches no tool of MCP server `{}`",
                        sel.server
                    ));
                }
            }
        }
        let tools = self.build(selectors, resolved);
        if let Ok(mut o) = self.inner.offers.lock() {
            o.entry(key.to_owned()).or_insert(tools).clone()
        } else {
            tools
        }
    }

    fn build(
        &self,
        selectors: &[McpSelector],
        resolved: Vec<(String, Tool)>,
    ) -> Vec<Arc<dyn AgentTool>> {
        let mut names = BTreeSet::new();
        let direct: Vec<McpTool> = resolved
            .into_iter()
            .map(|(server, tool)| {
                let mut name = model_tool_name(&server, &tool.name);
                let mut n = 2;
                while !names.insert(name.clone()) {
                    let suffix = format!("_{n}");
                    let base = model_tool_name(&server, &tool.name);
                    let cut = base.len().min(MAX_MODEL_NAME - suffix.len());
                    name = format!("{}{suffix}", &base[..cut]);
                    n += 1;
                }
                McpTool {
                    toolbox: self.clone(),
                    read_only: self.inner.opts.trusted_annotations.contains(&server)
                        && read_only_hint(&tool),
                    description: description(&server, &tool),
                    schema: input_schema(&tool),
                    server,
                    tool: tool.name,
                    name,
                }
            })
            .collect();
        let specs: Vec<ToolSpec> = direct.iter().map(|t| t.spec()).collect();
        let cost = estimate_tools(&specs);
        if !direct.is_empty() && cost <= self.inner.opts.schema_budget {
            return direct
                .into_iter()
                .map(|t| Arc::new(t) as Arc<dyn AgentTool>)
                .collect();
        }
        if !direct.is_empty() {
            tracing::info!(
                target: "harness_mcp::tools",
                tools = direct.len(),
                tokens = cost,
                budget = self.inner.opts.schema_budget,
                "MCP tool schemas exceed the budget; offering mcp_tools instead"
            );
        }
        vec![Arc::new(GatewayTool::new(self.clone(), selectors.to_vec())) as Arc<dyn AgentTool>]
    }

    /// The current definition of `tool` on `server`, re-reading the list
    /// first if the server announced a change. `searched` says the model
    /// named the tool through `mcp_tools` (the error then says to search).
    async fn current(&self, server: &str, tool: &str, searched: bool) -> Result<Tool, ToolError> {
        let listing = self.ensure(server).await.ok_or_else(|| {
            ToolError::new(
                ToolErrorKind::Exec,
                format!("MCP server `{server}` is not available. Tell the user; it is configured in harness.ini."),
            )
        })?;
        if let Some(t) = listing.tools.iter().find(|t| t.name == tool) {
            return Ok(t.clone());
        }
        Err(ToolError::new(
            ToolErrorKind::NotFound,
            match listing.error {
                Some(e) => format!(
                    "MCP server `{server}` is not answering ({e}). Try again later or tell the user."
                ),
                None if searched => format!(
                    "MCP server `{server}` does not offer `{tool}`. Search with `{GATEWAY_TOOL}` and call a tool exactly as listed."
                ),
                None => format!(
                    "MCP server `{server}` no longer offers `{tool}` (its tool list changed). Do not retry it; continue without it or tell the user."
                ),
            },
        ))
    }

    /// Calls `tool` on `server` and turns the result into tool output.
    async fn call(
        &self,
        ctx: &ToolContext,
        model_name: &str,
        server: &str,
        tool: &str,
        args: Value,
        searched: bool,
    ) -> Result<ToolOutput, ToolError> {
        let handle = self.inner.servers.get(server).cloned().ok_or_else(|| {
            ToolError::new(
                ToolErrorKind::Exec,
                format!("MCP server `{server}` is not defined in harness.ini. Tell the user."),
            )
        })?;
        self.current(server, tool, searched).await?;
        let result = handle.call_tool(tool, args).await.map_err(|e| {
            ToolError::new(
                ToolErrorKind::Exec,
                handle.redact(&format!(
                    "MCP server `{server}` failed to run `{tool}`: {e}"
                )),
            )
        })?;
        let (text, image) = render(&result, ctx.supports_images);
        let text = handle.redact(&text);
        if result.is_error {
            return Err(ToolError::new(
                ToolErrorKind::Exec,
                format!(
                    "`{tool}` reported an error: {}",
                    self.cap(ctx, model_name, text)
                ),
            ));
        }
        let text = self.cap(ctx, model_name, text);
        Ok(match image {
            Some((media_type, data)) => ToolOutput::Image {
                media_type,
                data,
                caption: text,
            },
            None => ToolOutput::Text(text),
        })
    }

    /// Applies the output budget.
    fn cap(&self, ctx: &ToolContext, model_name: &str, text: String) -> String {
        match &self.inner.opts.spill {
            Some(s) => s.apply(ctx, model_name, text),
            None if text.len() > DEFAULT_MAX_RESULT_BYTES => {
                let cut = text.floor_char_boundary(DEFAULT_MAX_RESULT_BYTES);
                format!(
                    "{}\n[output cut at {DEFAULT_MAX_RESULT_BYTES} of {} bytes]",
                    &text[..cut],
                    text.len()
                )
            }
            None => text,
        }
    }
}

/// Text of a tool result plus, when images are accepted, its first image.
fn render(r: &CallToolResult, images: bool) -> (String, Option<(String, Vec<u8>)>) {
    use base64::Engine as _;
    let mut parts = Vec::new();
    let mut image = None;
    for c in &r.content {
        let get = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default();
        match get("type") {
            "text" => parts.push(get("text").to_owned()),
            kind @ ("image" | "audio") => {
                let data = get("data");
                let mime = get("mimeType").to_owned();
                let decoded = base64::engine::general_purpose::STANDARD.decode(data).ok();
                let size = decoded.as_ref().map_or(0, Vec::len);
                if kind == "image"
                    && images
                    && image.is_none()
                    && let Some(bytes) = decoded
                {
                    image = Some((mime, bytes));
                    continue;
                }
                parts.push(format!("[{kind}: {mime}, {size} bytes, not shown]"));
            }
            "resource_link" => {
                parts.push(format!("[resource link: {} {}]", get("uri"), get("name")))
            }
            "resource" => {
                let res = c.get("resource").cloned().unwrap_or(Value::Null);
                let uri = res.get("uri").and_then(Value::as_str).unwrap_or_default();
                match res.get("text").and_then(Value::as_str) {
                    Some(t) => parts.push(format!("[resource {uri}]\n{t}")),
                    None => parts.push(format!("[resource {uri}: binary, not shown]")),
                }
            }
            other => parts.push(format!("[{other} content, not shown]")),
        }
    }
    if let Some(s) = &r.structured_content
        && parts.is_empty()
    {
        parts.push(serde_json::to_string_pretty(s).unwrap_or_default());
    }
    if parts.is_empty() && image.is_none() {
        parts.push("(no output)".into());
    }
    (parts.join("\n"), image)
}

/// One MCP tool offered under its own name.
struct McpTool {
    toolbox: McpToolbox,
    name: String,
    server: String,
    tool: String,
    description: String,
    schema: Value,
    read_only: bool,
}

#[async_trait]
impl AgentTool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Value {
        self.schema.clone()
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.toolbox
            .call(ctx, &self.name, &self.server, &self.tool, args, false)
            .await
    }
    fn mcp_target(&self, _args: &Value) -> Option<McpTarget> {
        Some(McpTarget {
            server: self.server.clone(),
            tool: self.tool.clone(),
        })
    }
}

/// The search-and-call tool standing in for many MCP tools.
struct GatewayTool {
    toolbox: McpToolbox,
    selectors: Vec<McpSelector>,
    description: String,
}

impl GatewayTool {
    fn new(toolbox: McpToolbox, selectors: Vec<McpSelector>) -> Self {
        let servers: BTreeSet<&str> = selectors.iter().map(|s| s.server.as_str()).collect();
        let servers: Vec<&str> = servers.into_iter().collect();
        let description = format!(
            "Find and run tools of MCP servers ({}). First `search` with a few words: it returns matching tools with their argument schemas (an empty search lists every tool). Then `call` one as `server/tool` with `arguments` matching its schema.",
            servers.join(", ")
        );
        Self {
            toolbox,
            selectors,
            description,
        }
    }

    fn selected(&self, server: &str, tool: &str) -> bool {
        self.selectors.iter().any(|s| s.matches(server, tool))
    }

    async fn search(&self, query: &str) -> String {
        self.toolbox.prepare(&self.selectors).await;
        let tools = self.toolbox.resolve(&self.selectors).await;
        if tools.is_empty() {
            return "No MCP tools are available right now (their servers may be down). Tell the user.".into();
        }
        let words: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        if words.is_empty() {
            let mut out = format!(
                "{} MCP tools (search to see their arguments):\n",
                tools.len()
            );
            for (s, t) in tools.iter().take(LIST_RESULTS) {
                let d = t
                    .description
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("");
                out.push_str(&format!(
                    "- {s}/{}: {}\n",
                    t.name,
                    robustness::truncate_chars(d, 100)
                ));
            }
            if tools.len() > LIST_RESULTS {
                out.push_str(&format!(
                    "... and {} more; search to narrow down.\n",
                    tools.len() - LIST_RESULTS
                ));
            }
            return out;
        }
        let mut scored: Vec<(usize, &(String, Tool))> = tools
            .iter()
            .map(|st| {
                let (s, t) = st;
                let name = format!("{s} {}", t.name).to_lowercase();
                let desc = t.description.as_deref().unwrap_or("").to_lowercase();
                let score = words
                    .iter()
                    .map(|w| {
                        usize::from(name.contains(w.as_str())) * 3
                            + usize::from(desc.contains(w.as_str()))
                    })
                    .sum();
                (score, st)
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        if scored.is_empty() {
            return format!(
                "No MCP tool matches `{query}`. Search with other words, or with an empty `search` to list all {} tools.",
                tools.len()
            );
        }
        let mut out = String::new();
        for (_, (s, t)) in scored.iter().take(SEARCH_RESULTS) {
            out.push_str(&format!(
                "## {s}/{}\n{}\narguments: {}\n\n",
                t.name,
                robustness::truncate_chars(
                    t.description.as_deref().unwrap_or(""),
                    MAX_DESCRIPTION_CHARS
                ),
                serde_json::to_string(&input_schema(t)).unwrap_or_default()
            ));
        }
        if scored.len() > SEARCH_RESULTS {
            out.push_str(&format!(
                "{} more tools match; add words to narrow the search.\n",
                scored.len() - SEARCH_RESULTS
            ));
        }
        out.push_str(&format!(
            "Call one with {GATEWAY_TOOL} {{\"call\": \"server/tool\", \"arguments\": {{...}}}}."
        ));
        out
    }
}

#[async_trait]
impl AgentTool for GatewayTool {
    fn name(&self) -> &str {
        GATEWAY_TOOL
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "search": {"type": "string", "description": "Words to look for in tool names and descriptions."},
                "call": {"type": "string", "description": "Tool to run, as server/tool."},
                "arguments": {"type": "object", "description": "Arguments for `call`."}
            },
            "additionalProperties": false
        })
    }
    fn read_only(&self) -> bool {
        false
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let call = args.get("call").and_then(Value::as_str);
        let search = args.get("search").and_then(Value::as_str);
        let Some(target) = call else {
            return match search {
                Some(q) => Ok(ToolOutput::Text(self.search(q).await)),
                None => Err(ToolError::invalid_args(
                    ToolErrorMessage::new(
                        GATEWAY_TOOL,
                        "neither `search` nor `call` was given",
                        "Pass `search` (a few words) to find tools, then `call` (server/tool) with `arguments`.",
                    )
                    .to_string(),
                )),
            };
        };
        let Some((server, tool)) = split_ref(target).filter(|(s, t)| self.selected(s, t)) else {
            return Err(ToolError::invalid_args(
                ToolErrorMessage::new(
                    GATEWAY_TOOL,
                    format!("`{target}` is not one of the MCP tools this agent may use"),
                    "Search first and call a tool exactly as listed (server/tool).",
                )
                .to_string(),
            ));
        };
        let def = self.toolbox.current(server, tool, true).await?;
        let schema = input_schema(&def);
        let mut arguments = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let mut repairs = Vec::new();
        if let Err(issues) = robustness::validate(&schema, &mut arguments, &mut repairs) {
            return Err(ToolError::invalid_args(
                ToolErrorMessage::invalid_args(&format!("{server}/{tool}"), &issues, &schema)
                    .to_string(),
            ));
        }
        self.toolbox
            .call(ctx, GATEWAY_TOOL, server, tool, arguments, true)
            .await
    }
    fn mcp_target(&self, args: &Value) -> Option<McpTarget> {
        let (server, tool) = split_ref(args.get("call")?.as_str()?)?;
        self.selected(server, tool).then(|| McpTarget {
            server: server.to_owned(),
            tool: tool.to_owned(),
        })
    }
}

/// The key an agent's offer is kept under.
fn offer_key(agent: &Agent, selectors: &[McpSelector]) -> String {
    let sels: Vec<String> = selectors.iter().map(ToString::to_string).collect();
    format!("{}|{}", agent.name, sels.join(","))
}

fn selectors_for(agent: &Agent, extra: &[McpSelector]) -> Vec<McpSelector> {
    let mut v = agent.mcp_tools.clone();
    for s in extra {
        if !v.contains(s) {
            v.push(s.clone());
        }
    }
    v
}

#[async_trait]
impl ExtraTools for McpToolbox {
    async fn prepare(&self, agent: &Agent, extra: &[McpSelector]) {
        let sels = selectors_for(agent, extra);
        if sels.is_empty() {
            return;
        }
        McpToolbox::prepare(self, &sels).await;
        // Build (and freeze) the offer now, while we may wait.
        self.offer(&offer_key(agent, &sels), &sels).await;
    }

    fn tools(&self, agent: &Agent, extra: &[McpSelector]) -> Vec<Arc<dyn AgentTool>> {
        let sels = selectors_for(agent, extra);
        if sels.is_empty() {
            return Vec::new();
        }
        let key = offer_key(agent, &sels);
        if let Some(t) = self
            .inner
            .offers
            .lock()
            .ok()
            .and_then(|o| o.get(&key).cloned())
        {
            return t;
        }
        // Not prepared: offer the search-and-call tool, which loads lists
        // on use and keeps the prefix stable.
        if let Ok(mut s) = self.inner.selected.lock() {
            s.extend(sels.iter().cloned());
        }
        let tools = vec![Arc::new(GatewayTool::new(self.clone(), sels)) as Arc<dyn AgentTool>];
        if let Ok(mut o) = self.inner.offers.lock() {
            o.entry(key).or_insert(tools).clone()
        } else {
            tools
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names_are_provider_safe_and_bounded() {
        assert_eq!(
            model_tool_name("github", "create_issue"),
            "mcp__github__create_issue"
        );
        assert_eq!(
            model_tool_name("db", "admin.tools.list"),
            "mcp__db__admin_tools_list"
        );
        let long = model_tool_name("server", &"x".repeat(200));
        assert_eq!(long.len(), MAX_MODEL_NAME);
        assert!(long.starts_with("mcp__server__xxx"));
        assert_ne!(long, model_tool_name("server", &"x".repeat(201)));
        assert!(harness_policy::mcp::is_model_tool_name(&long));
    }

    #[test]
    fn results_render_text_media_resources_and_structured_content() {
        let r = CallToolResult::from_value(&json!({"content": [
            {"type": "text", "text": "hello"},
            {"type": "image", "data": "aGk=", "mimeType": "image/png"},
            {"type": "resource_link", "uri": "file:///a.rs", "name": "a.rs"},
            {"type": "resource", "resource": {"uri": "file:///b.txt", "text": "body"}},
            {"type": "weird"}
        ]}));
        let (text, image) = render(&r, false);
        assert!(image.is_none());
        assert_eq!(
            text,
            "hello\n[image: image/png, 2 bytes, not shown]\n[resource link: file:///a.rs a.rs]\n[resource file:///b.txt]\nbody\n[weird content, not shown]"
        );
        let (text, image) = render(&r, true);
        assert_eq!(image, Some(("image/png".into(), b"hi".to_vec())));
        assert!(!text.contains("[image"));
        let s = CallToolResult::from_value(&json!({"structuredContent": {"a": 1}}));
        assert!(render(&s, false).0.contains("\"a\": 1"));
        let empty = CallToolResult::from_value(&json!({}));
        assert_eq!(render(&empty, false).0, "(no output)");
    }

    #[test]
    fn schemas_are_always_object_schemas() {
        let t = Tool::from_value(&json!({"name": "t"})).unwrap();
        assert_eq!(input_schema(&t), json!({"type": "object"}));
        let t = Tool::from_value(&json!({"name": "t", "inputSchema": {"properties": {}}})).unwrap();
        assert_eq!(input_schema(&t)["type"], "object");
    }
}
